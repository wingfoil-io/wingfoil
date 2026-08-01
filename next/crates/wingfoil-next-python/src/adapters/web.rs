//! Python bindings for the wingfoil-next **web** adapter
//! ([`wingfoil_next::adapters::web`]).
//!
//! One class, no free functions:
//!
//! | Python                              | Rust                        |
//! |-------------------------------------|-----------------------------|
//! | `WebServer(addr, …)`                | [`WebServer::bind`] + `start` |
//! | `server.port()` / `codec_name()`    | [`WebServer::port`] / [`codec`](WebServer::codec) |
//! | `server.sub(graph, topic)`          | [`web_sub`]                 |
//! | `server.pub(stream, topic)`         | [`WebSinkOps::web_pub`]     |
//! | `server.pub_bursts(stream, topic)`  | [`WebBurstSinkOps::web_pub_bursts`] |
//! | `server.stop()`                     | [`WebServer::stop`]         |
//!
//! # Why this one is hand-written
//!
//! The server is a **stateful handle** with a lifecycle — bound, served,
//! published/subscribed against repeatedly, then stopped — which
//! `#[pyadapter]` has no shape for. It is the third of that kind, after
//! prometheus's exporter and `fix_connect_tls`, and the first whose handle
//! wires a *source*: `sub` therefore takes the `Graph` explicitly, since
//! [`web_sub`] needs a builder to create the node on.
//!
//! # The payload edge
//!
//! Python values marshal through **`serde_json::Value`**, which the adapter
//! serialises with whichever codec the server was built with (bincode or JSON),
//! and which the browser client decodes the same way — so a Python publisher is
//! wire-compatible with a Rust one. `None` / `bool` / `int` / `float` / `str` /
//! `list` / `dict` map to the obvious JSON shapes.
//!
//! `bytes` become a JSON **array of ints**, as legacy did, so they stay
//! wire-compatible with a Rust peer publishing a `Vec<u8>`. That is
//! deliberately asymmetric: a subscription decodes such a frame back to a
//! `list` of ints, not to `bytes`, because nothing on the wire distinguishes
//! the two.
//!
//! `sub` is burst-shaped, so each tick yields a **`list`** of the frames that
//! arrived between graph cycles. `pub` puts one value per frame on the wire;
//! `pub_bursts` puts a whole same-instant group on the wire as one array frame,
//! atomically, so a lossy client drop can never split a timestamp.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Marshaling fails loudly.** Legacy's `py_to_serde(…).unwrap_or(Null)`
//!    turned an unsupported value — an object, a set, a non-`str` dict key —
//!    into a published JSON `null`. Here the run aborts naming the type.
//! 2. **Publishing is a server method, not a stream method.** Legacy had
//!    `stream.web_pub(server, topic)`; the handle owns the topic registry, so
//!    `server.pub(stream, topic)` keeps every web entry point in one place —
//!    the same free-fn/handle uniformity the other bindings follow.
//! 3. **New capabilities**: `pub_bursts` (classic had no burst overload —
//!    callers converted by hand), TLS (`cert_path` / `key_path`), `stop()`, and
//!    the `is_historical_noop` / `is_tls` introspection.

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString};
use serde_json::{Map, Number, Value};
use wingfoil_next::adapters::web::{
    CodecKind, WebBurstSinkOps, WebServer, WebServerBuilder, WebSinkOps, web_sub,
};
use wingfoil_next::prelude::{Burst, Stream, StreamOps};

use crate::{Graph, PyElement};

/// The error-message prefix for the publish path's marshaling failures.
const WHO: &str = "web_pub";

// ---------------------------------------------------------------------------
// The payload edge: Python <-> serde_json::Value.
// ---------------------------------------------------------------------------

/// Marshal one Python value into the JSON value the codec serialises.
///
/// Fails loudly on anything with no JSON shape — legacy turned those into a
/// published `null`, which is indistinguishable from a deliberate `None`.
fn to_json(obj: &Bound<'_, PyAny>) -> Result<Value> {
    if obj.is_none() {
        return Ok(Value::Null);
    }
    // `bool` before `int`: Python's bool *is* an int, and the JSON shapes differ.
    if obj.is_instance_of::<PyBool>() {
        return Ok(Value::Bool(obj.extract::<bool>()?));
    }
    if obj.is_instance_of::<PyInt>() {
        if let Ok(v) = obj.extract::<i64>() {
            return Ok(Value::Number(v.into()));
        }
        if let Ok(v) = obj.extract::<u64>() {
            return Ok(Value::Number(v.into()));
        }
        // Beyond u64: fall back to a float, as legacy did, rather than refuse.
        return number(obj.extract::<f64>()?);
    }
    if obj.is_instance_of::<PyFloat>() {
        return number(obj.extract::<f64>()?);
    }
    if obj.is_instance_of::<PyString>() {
        return Ok(Value::String(obj.extract::<String>()?));
    }
    if let Ok(bytes) = obj.cast::<PyBytes>() {
        // An array of ints — wire-compatible with a Rust peer's `Vec<u8>`.
        return Ok(Value::Array(
            bytes
                .as_bytes()
                .iter()
                .map(|b| Value::Number(u64::from(*b).into()))
                .collect(),
        ));
    }
    if let Ok(list) = obj.cast::<PyList>() {
        return list.iter().map(|item| to_json(&item)).collect();
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = Map::new();
        for (key, value) in dict.iter() {
            let key = key.extract::<String>().map_err(|_| {
                anyhow!("{WHO}: dict keys must be str for a JSON-compatible publish")
            })?;
            map.insert(key, to_json(&value)?);
        }
        return Ok(Value::Object(map));
    }
    bail!(
        "{WHO}: unsupported value type '{}'; expected None, bool, int, float, str, \
         bytes, list or dict",
        obj.get_type()
    )
}

/// A JSON number, rejecting the non-finite floats JSON cannot represent.
fn number(value: f64) -> Result<Value> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| anyhow!("{WHO}: {value} has no JSON representation (NaN and ±inf do not)"))
}

/// Marshal one erased stream value on its way to the wire.
fn element_to_json(elem: &PyElement) -> Result<Value> {
    if elem.is_none() {
        bail!("{WHO}: stream value is empty (the upstream produced no value)");
    }
    Python::attach(|py| to_json(elem.object().bind(py)))
}

impl From<Value> for PyElement {
    fn from(value: Value) -> Self {
        Python::attach(|py| PyElement::new(to_py(py, &value)))
    }
}

/// Rebuild a Python value from a decoded frame.
///
/// Infallible: every JSON shape has a Python counterpart, and the only
/// conversions involved (numbers, strings, containers) cannot fail.
fn to_py(py: Python<'_>, value: &Value) -> Py<PyAny> {
    /// Box a scalar. Primitive `IntoPyObject` conversions are infallible.
    macro_rules! boxed {
        ($v:expr) => {
            $v.into_py_any(py)
                .expect("invariant: scalar -> PyObject conversion is infallible")
        };
    }
    use pyo3::IntoPyObjectExt;
    match value {
        Value::Null => py.None(),
        Value::Bool(v) => boxed!(*v),
        Value::Number(n) => {
            if let Some(v) = n.as_i64() {
                boxed!(v)
            } else if let Some(v) = n.as_u64() {
                boxed!(v)
            } else if let Some(v) = n.as_f64() {
                boxed!(v)
            } else {
                // serde_json numbers are always one of the three above.
                py.None()
            }
        }
        Value::String(v) => boxed!(v.as_str()),
        Value::Array(items) => {
            let values: Vec<Py<PyAny>> = items.iter().map(|v| to_py(py, v)).collect();
            PyList::new(py, values)
                .expect("invariant: building a PyList from owned values cannot fail")
                .into_any()
                .unbind()
        }
        Value::Object(fields) => {
            let dict = PyDict::new(py);
            for (key, value) in fields {
                dict.set_item(key, to_py(py, value))
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            }
            dict.into_any().unbind()
        }
    }
}

/// The codec selector, a string rather than a `#[pyclass]` enum.
fn codec_kind(name: &str) -> Result<CodecKind> {
    match name {
        "bincode" => Ok(CodecKind::Bincode),
        "json" => Ok(CodecKind::Json),
        other => bail!("web: unknown codec '{other}'; expected 'bincode' or 'json'"),
    }
}

// ---------------------------------------------------------------------------
// The handle.
// ---------------------------------------------------------------------------

/// An HTTP + WebSocket server streaming graph values to browsers.
///
/// ```python
/// server = wf.WebServer("127.0.0.1:0", codec="json")
/// g = wf.Graph()
/// server.pub(g.counter(period_nanos=1_000_000_000), "ticks")
/// clicks = server.sub(g, "clicks")
/// g.run(realtime=True, duration_nanos=10_000_000_000)
/// ```
#[pyclass(name = "WebServer", unsendable)]
pub struct PyWebServer {
    server: WebServer,
}

#[pymethods]
impl PyWebServer {
    /// Bind `addr` and start the server.
    ///
    /// `addr` is e.g. `"0.0.0.0:8080"`, or `"127.0.0.1:0"` for an OS-assigned
    /// port. `codec` is `"bincode"` (the default) or `"json"` — whichever the
    /// browser client is configured for. `static_dir` serves files under
    /// `GET /` alongside the WebSocket endpoint.
    ///
    /// `historical=True` builds a **no-op** server: no port is bound, and both
    /// `pub` and `sub` become no-ops, so a backtest can run the same graph
    /// unmodified. (A *live* server under a historical run still streams its
    /// output to the browser — that is what powers replay visualisation; only
    /// `sub` stays silent, since live input has no place in a deterministic
    /// replay.)
    ///
    /// `cert_path` / `key_path` terminate TLS with a PEM chain and private key,
    /// so clients connect over `https://` / `wss://`. Both must be given
    /// together. The files are read here, so a missing or malformed PEM raises
    /// alongside a bind error rather than failing later inside the server.
    #[new]
    #[pyo3(signature = (
        addr, codec = "bincode".to_string(), static_dir = None, historical = false,
        cert_path = None, key_path = None,
    ))]
    fn new(
        addr: String,
        codec: String,
        static_dir: Option<String>,
        historical: bool,
        cert_path: Option<String>,
        key_path: Option<String>,
    ) -> PyResult<Self> {
        Self::build(addr, codec, static_dir, historical, cert_path, key_path)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))
    }

    /// The bound port — `0` for a `historical=True` no-op server.
    fn port(&self) -> u16 {
        self.server.port()
    }

    /// The wire codec in use: `"bincode"` or `"json"`.
    fn codec_name(&self) -> &'static str {
        match self.server.codec() {
            CodecKind::Bincode => "bincode",
            CodecKind::Json => "json",
        }
    }

    /// Whether this is a `historical=True` no-op server (nothing is bound).
    fn is_historical_noop(&self) -> bool {
        self.server.is_historical_noop()
    }

    /// Whether the server terminates TLS.
    fn is_tls(&self) -> bool {
        self.server.is_tls()
    }

    /// Shut the server down, closing the listener and every connection.
    ///
    /// Idempotent, and implied by dropping the server; call it to release the
    /// port at a known point rather than at garbage collection.
    fn stop(&mut self) {
        self.server.stop();
    }

    /// Subscribe to the frames clients send on `topic`.
    ///
    /// Each tick yields a `list` of decoded values — the frames that arrived
    /// between graph cycles, losslessly grouped. A decode failure aborts the
    /// run. The stream never ticks under a historical run (see the constructor).
    ///
    /// Takes the `Graph` because this creates a source node on it; the other
    /// two methods wire onto the stream they are given.
    fn sub(&self, graph: PyRef<'_, Graph>, topic: String) -> PyResult<crate::Stream> {
        let object = graph.object();
        let frames: Stream<Burst<Value>> = web_sub(object.builder(), &self.server, topic)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))?;
        Ok(crate::Stream::from(
            object.erase_burst_source::<Value>(frames),
        ))
    }

    /// Publish every tick of `stream` on `topic`, one value per frame.
    ///
    /// A value with no JSON shape — an arbitrary object, a set, a dict with
    /// non-`str` keys, a non-finite float — aborts the run rather than being
    /// published as `null`. Returns a terminal stream whose value is `None`; it
    /// must stay wired into the graph for anything to be sent.
    #[pyo3(name = "pub")]
    fn publish(&self, stream: PyRef<'_, crate::Stream>, topic: String) -> PyResult<crate::Stream> {
        let object = stream.object();
        let values: Stream<Value> = object.typed_input::<PyElement>().try_map(element_to_json);
        let sink = values
            .web_pub(&self.server, topic)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))?;
        Ok(crate::Stream::from(object.erased_output::<()>(sink)))
    }

    /// Publish each tick of `stream` on `topic` as **one array frame**.
    ///
    /// A tick whose value is a `list` or `tuple` is a multi-value burst; any
    /// other value is a one-element one. The whole same-instant group crosses
    /// the wire atomically, so a lossy client drop can never split a timestamp
    /// — the browser client surfaces it whole via `subscribeBurst`.
    ///
    /// Legacy had no burst overload; callers converted by hand.
    #[pyo3(name = "pub_bursts")]
    fn publish_bursts(
        &self,
        stream: PyRef<'_, crate::Stream>,
        topic: String,
    ) -> PyResult<crate::Stream> {
        let object = stream.object();
        let values: Stream<Burst<Value>> =
            object
                .typed_burst_input::<PyElement>()
                .try_map(|burst: &Burst<PyElement>| {
                    // One attach for the whole burst; the per-element ones then
                    // short-circuit on the thread-local count.
                    Python::attach(|_py| burst.iter().map(element_to_json).collect())
                });
        let sink = values
            .web_pub_bursts(&self.server, topic)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))?;
        Ok(crate::Stream::from(object.erased_output::<()>(sink)))
    }
}

impl PyWebServer {
    /// The fallible half of the constructor, kept in `anyhow` so the argument
    /// validation reads the same as every other binding's.
    fn build(
        addr: String,
        codec: String,
        static_dir: Option<String>,
        historical: bool,
        cert_path: Option<String>,
        key_path: Option<String>,
    ) -> Result<Self> {
        let mut builder: WebServerBuilder = WebServer::bind(addr).codec(codec_kind(&codec)?);
        if let Some(dir) = static_dir {
            builder = builder.serve_static(dir);
        }
        builder = match (cert_path, key_path) {
            (Some(cert), Some(key)) => builder.tls(cert, key),
            (None, None) => builder,
            _ => bail!("web: cert_path and key_path must be given together, or neither"),
        };
        let server = if historical {
            builder.start_historical()
        } else {
            builder.start()
        }?;
        Ok(Self { server })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::IntoPyObjectExt;

    fn json(py: Python<'_>, source: &str) -> Result<Value> {
        let obj = py.eval(&std::ffi::CString::new(source).unwrap(), None, None)?;
        to_json(&obj)
    }

    #[test]
    fn the_scalar_shapes_marshal() {
        Python::attach(|py| {
            assert_eq!(Value::Null, json(py, "None").unwrap());
            assert_eq!(Value::Bool(true), json(py, "True").unwrap());
            assert_eq!(Value::from(7), json(py, "7").unwrap());
            assert_eq!(Value::from(1.5), json(py, "1.5").unwrap());
            assert_eq!(Value::from("hi"), json(py, "'hi'").unwrap());
        });
    }

    #[test]
    fn a_bool_does_not_marshal_as_an_int() {
        // Python's `bool` is an `int` subclass; the JSON shapes differ.
        Python::attach(|py| {
            assert_eq!(Value::Bool(false), json(py, "False").unwrap());
        });
    }

    #[test]
    fn containers_marshal_recursively() {
        Python::attach(|py| {
            assert_eq!(
                serde_json::json!({"a": [1, 2], "b": {"c": None::<u8>}}),
                json(py, "{'a': [1, 2], 'b': {'c': None}}").unwrap()
            );
        });
    }

    #[test]
    fn bytes_marshal_as_an_array_of_ints() {
        // Wire-compatible with a Rust peer publishing a `Vec<u8>`.
        Python::attach(|py| {
            assert_eq!(
                serde_json::json!([1, 42]),
                json(py, "bytes([1, 42])").unwrap()
            );
        });
    }

    #[test]
    fn an_unsupported_type_errors_rather_than_publishing_null() {
        Python::attach(|py| {
            let err = json(py, "{1, 2}").unwrap_err();
            assert!(err.to_string().contains("unsupported value type"), "{err}");
        });
    }

    #[test]
    fn a_non_str_dict_key_errors() {
        Python::attach(|py| {
            let err = json(py, "{1: 'a'}").unwrap_err();
            assert!(err.to_string().contains("dict keys must be str"), "{err}");
        });
    }

    #[test]
    fn a_non_finite_float_errors() {
        Python::attach(|py| {
            let err = json(py, "float('nan')").unwrap_err();
            assert!(err.to_string().contains("no JSON representation"), "{err}");
        });
    }

    #[test]
    fn an_empty_element_errors() {
        let err = element_to_json(&PyElement::default()).unwrap_err();
        assert!(err.to_string().contains("stream value is empty"), "{err}");
    }

    #[test]
    fn a_frame_rebuilds_the_python_value() {
        Python::attach(|py| {
            let element = PyElement::from(serde_json::json!({"sym": "AAPL", "px": [1, 2.5]}));
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("AAPL", get("sym").extract::<String>().unwrap());
            assert_eq!(vec![1.0, 2.5], get("px").extract::<Vec<f64>>().unwrap());
        });
    }

    #[test]
    fn a_null_frame_rebuilds_as_none() {
        Python::attach(|py| {
            let element = PyElement::from(Value::Null);
            assert!(element.object().bind(py).is_none());
        });
    }

    #[test]
    fn every_value_round_trips_through_both_directions() {
        Python::attach(|py| {
            let original = serde_json::json!({"a": [true, 1, 1.5, "s", None::<u8>]});
            let element = PyElement::from(original.clone());
            let obj = element.object().bind(py).clone();
            assert_eq!(original, to_json(&obj).unwrap());
        });
    }

    #[test]
    fn the_codec_selector_is_a_string() {
        assert!(matches!(codec_kind("bincode"), Ok(CodecKind::Bincode)));
        assert!(matches!(codec_kind("json"), Ok(CodecKind::Json)));
        let err = codec_kind("protobuf").unwrap_err();
        assert!(err.to_string().contains("expected 'bincode' or 'json'"));
    }

    #[test]
    fn credentials_must_be_given_together() {
        let build = |cert: Option<&str>, key: Option<&str>| {
            PyWebServer::build(
                "127.0.0.1:0".into(),
                "json".into(),
                None,
                true,
                cert.map(str::to_string),
                key.map(str::to_string),
            )
        };
        assert!(build(None, None).is_ok());
        assert!(build(Some("c.pem"), None).is_err());
        assert!(build(None, Some("k.pem")).is_err());
    }

    #[test]
    fn a_large_int_falls_back_to_a_float() {
        Python::attach(|py| {
            let value = json(py, "2 ** 70").unwrap();
            assert!(value.as_f64().unwrap() > 1e21);
        });
    }

    #[test]
    fn an_i64_stays_exact() {
        Python::attach(|py| {
            let element = (-5_i64).into_py_any(py).unwrap();
            assert_eq!(Value::from(-5), to_json(element.bind(py)).unwrap());
        });
    }
}
