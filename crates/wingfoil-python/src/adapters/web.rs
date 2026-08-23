//! Python bindings for the wingfoil **web** adapter
//! ([`wingfoil::adapters::web`]).
//!
//! One class, no free functions:
//!
//! | Python                              | Rust                        |
//! |-------------------------------------|-----------------------------|
//! | `WebServer(addr, …)`                | [`WebServer::bind`] + `start` |
//! | `server.port()` / `codec_name()`    | [`WebServer::port`] / [`codec`](WebServer::codec) |
//! | `server.delivery_name()`            | [`WebServer::delivery`]     |
//! | `server.lossless_stall_timeout_secs()` | [`WebServer::lossless_stall_timeout`] |
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
//! Python values marshal through **`serde_json::Value`**: `None` / `bool` /
//! `int` / `float` / `str` / `list` / `dict` map to the obvious JSON shapes,
//! and `bytes` become an **array of ints**, as legacy did.
//!
//! `Value` is a *schema-less* shape, and that — not the adapter — is what
//! decides which codec and which peers actually work.
//!
//! # The codec is not a free choice: `codec="json"` is the interoperable one
//!
//! bincode is schema-driven and non-self-describing, so it needs the Rust type
//! at both ends. It therefore **cannot deserialize into a `serde_json::Value`
//! at all**: `Value`'s `Deserialize` asks for `deserialize_any`, which bincode
//! refuses for every input, of every shape. That single fact settles the
//! matrix (`sub` ← anything is what issue #821's Rust/wasm half hit from the
//! other side):
//!
//! | From Python | to | `codec="json"` | `codec="bincode"` |
//! |---|---|---|---|
//! | `pub` / `pub_bursts` | a typed Rust `web_sub::<T>` | yes | **only when `T`'s bincode layout happens to coincide with `Value`'s** — a scalar or a homogeneous sequence does; a `dict` reaches a `struct` as **silent garbage** (#821), and `bytes` reach a `Vec<u8>` as garbage, since `Value` writes each element as a `u64` |
//! | `pub` / `pub_bursts` | another Python `sub` | yes | **no** — nothing can decode it |
//! | `pub` / `pub_bursts` | the browser (`@wingfoil/client`) | yes | **no** — the client rejects bincode data payloads outright |
//! | `sub` | any peer at all | yes | **rejected at wiring**, with a message naming the fix |
//!
//! So: **`codec="json"` is the only setting every peer can talk to**, and the
//! only one a browser or a second Python process can talk to at all.
//! `codec="bincode"` — the default, matching the Rust adapter's — is
//! publish-only from Python, and only to a Rust peer whose `T` is a scalar or
//! a sequence of one width. The envelope and `$ctrl` control frames are
//! unaffected either way: they have fixed shapes both sides know.
//!
//! The `bytes` → array-of-ints mapping is wire-compatible with a Rust peer's
//! `Vec<u8>` **under JSON** — JSON has one number type, so `[1, 42]` reads
//! straight back as a `Vec<u8>`. It is deliberately asymmetric: a subscription
//! decodes such a frame back to a `list` of ints, not to `bytes`, because
//! nothing on the wire distinguishes the two.
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
//! 3. **New capabilities**: `pub_bursts` (legacy had no burst overload —
//!    callers converted by hand), TLS (`cert_path` / `key_path`), `stop()`, and
//!    the `is_historical_noop` / `is_tls` introspection.
//! 4. **`sub` refuses `codec="bincode"` at wiring.** Legacy accepted the call
//!    and then aborted the run on the *first frame* with bincode's
//!    `deserialize_any` complaint — a failure with no bearing on the frame that
//!    triggered it. It cannot decode any frame, so the diagnosis belongs at the
//!    call that chose the codec.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString};
use serde_json::{Map, Number, Value};
use wingfoil::adapters::web::{
    CodecKind, Delivery, WebBurstSinkOps, WebServer, WebServerBuilder, WebSinkOps, web_sub,
};
use wingfoil::prelude::{Burst, Stream, StreamOps};

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
        // An array of ints — wire-compatible with a Rust peer's `Vec<u8>`
        // under JSON. Under bincode it is not: each element goes out as a
        // `u64`, which a `Vec<u8>` peer misreads (see the module docs).
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

/// The delivery selector, a string rather than a `#[pyclass]` enum — same
/// treatment as [`codec_kind`], for the same reason.
fn delivery_kind(name: &str) -> Result<Delivery> {
    match name {
        "auto" => Ok(Delivery::Auto),
        "lossy" => Ok(Delivery::Lossy),
        "lossless" => Ok(Delivery::Lossless),
        other => {
            bail!("web: unknown delivery '{other}'; expected 'auto', 'lossy' or 'lossless'")
        }
    }
}

/// The lossless stall bound, rejecting a value `Duration` cannot represent.
/// Seconds as an `f64` is the tree's convention for a Python-facing timeout
/// (aeron's `timeout_secs`), and it validates the same way.
fn lossless_stall_timeout(secs: f64) -> Result<Duration> {
    if !secs.is_finite() || secs <= 0.0 {
        bail!("web: lossless_stall_timeout_secs must be a finite, positive number, got {secs}");
    }
    Duration::try_from_secs_f64(secs).map_err(|_| {
        anyhow!("web: lossless_stall_timeout_secs is too large for a Duration, got {secs}")
    })
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
    /// port. `static_dir` serves files under `GET /` alongside the WebSocket
    /// endpoint.
    ///
    /// `codec` is `"bincode"` (the default, matching the Rust adapter) or
    /// `"json"`, and **it is not a free choice from Python**. Python payloads
    /// marshal through a schema-less `serde_json::Value`, which bincode cannot
    /// deserialize at all, so `"bincode"` is publish-only here — `sub` rejects
    /// it, a browser rejects it, and a second Python process cannot read it. Use
    /// `"json"` for anything but publishing scalars to a typed Rust peer; see
    /// the *"The codec is not a free choice"* table in [the module docs](self).
    ///
    /// `historical=True` builds a **no-op** server: no port is bound, and both
    /// `pub` and `sub` become no-ops, so a backtest can run the same graph
    /// unmodified. (A *live* server under a historical run still streams its
    /// output to the browser — that is what powers replay visualisation; only
    /// `sub` stays silent, since live input has no place in a deterministic
    /// replay.)
    ///
    /// `delivery` decides what happens when a browser cannot keep up:
    /// `"auto"` (the default) drops frames under a realtime run and paces the
    /// graph to the slowest subscriber under a historical one, `"lossy"` always
    /// drops, `"lossless"` always paces. `"auto"` is what you want: dropping is
    /// right in real time, where the alternative is a frozen tab stalling a
    /// live graph, and wrong in a replay, where there is no live clock to fall
    /// behind and dropping just corrupts what the browser draws.
    ///
    /// `lossless_stall_timeout_secs` bounds how long a lossless publish waits
    /// on a browser whose outbound queue is full before deciding it is gone,
    /// closing its connection so it can reconnect (default 30 s). It has no
    /// effect under `delivery="lossy"`, which never waits. It bounds *death*,
    /// not slowness — the wait is for one slot in a 1024-deep queue, so a live
    /// client draining anything at all never trips it.
    ///
    /// `cert_path` / `key_path` terminate TLS with a PEM chain and private key,
    /// so clients connect over `https://` / `wss://`. Both must be given
    /// together. The files are read here, so a missing or malformed PEM raises
    /// alongside a bind error rather than failing later inside the server.
    #[new]
    #[pyo3(signature = (
        addr, codec = "bincode".to_string(), static_dir = None, historical = false,
        cert_path = None, key_path = None, delivery = "auto".to_string(),
        lossless_stall_timeout_secs = 30.0,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        addr: String,
        codec: String,
        static_dir: Option<String>,
        historical: bool,
        cert_path: Option<String>,
        key_path: Option<String>,
        delivery: String,
        lossless_stall_timeout_secs: f64,
    ) -> PyResult<Self> {
        Self::build(
            addr,
            codec,
            static_dir,
            historical,
            cert_path,
            key_path,
            delivery,
            lossless_stall_timeout_secs,
        )
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

    /// The configured delivery policy: `"auto"`, `"lossy"` or `"lossless"`.
    ///
    /// `"auto"` reads back as `"auto"` — which of the other two it means is a
    /// property of the run, decided when a graph starts, not of the server.
    fn delivery_name(&self) -> &'static str {
        match self.server.delivery() {
            Delivery::Auto => "auto",
            Delivery::Lossy => "lossy",
            Delivery::Lossless => "lossless",
        }
    }

    /// The lossless stall bound in seconds — see the constructor.
    fn lossless_stall_timeout_secs(&self) -> f64 {
        self.server.lossless_stall_timeout().as_secs_f64()
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
    /// **Requires `codec="json"`, and raises otherwise** — including on a
    /// `historical=True` no-op server, whose whole point is that the same graph
    /// also runs live. This is not a policy choice: frames decode into a
    /// schema-less `serde_json::Value`, and bincode refuses that for every
    /// input, so a bincode subscription has no decodable frame from any peer.
    ///
    /// Takes the `Graph` because this creates a source node on it; the other
    /// two methods wire onto the stream they are given.
    fn sub(&self, graph: PyRef<'_, Graph>, topic: String) -> PyResult<crate::Stream> {
        Self::require_decodable_codec(self.server.codec())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))?;
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
    ///
    /// Under `codec="bincode"` the only peer that can read these frames is a
    /// Rust `web_sub::<T>` whose `T` has the same bincode layout the value's
    /// `serde_json::Value` produces — true of a scalar or a sequence of one
    /// width, **false of a `dict` against a `struct`**, which arrives as silent
    /// garbage (#821). No check can catch that here: the adapter never sees the
    /// peer's type, and a `dict` against a `HashMap` peer is genuinely fine. Use
    /// `codec="json"` unless the peer is that Rust scalar case; see [the module
    /// docs](self).
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
    /// The codec caveat on [`publish`](Self::publish) applies unchanged; the
    /// browser leg of it needs `codec="json"` outright.
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
    /// Reject `codec="bincode"` on the subscribe path.
    ///
    /// `sub` is `web_sub::<serde_json::Value>`, and `Value`'s `Deserialize`
    /// asks for `deserialize_any` — which bincode, not being self-describing,
    /// refuses for every input of every shape. So there is no frame, from any
    /// peer, that a bincode subscription could decode: the alternative to this
    /// check is one abort per frame, mid-run, quoting a serde internal.
    ///
    /// Deliberately *not* mirrored on the publish path — a bincode publish has
    /// real working peers (a Rust `web_sub::<f64>`, `<String>`, `<Vec<f64>>`),
    /// and which ones work depends on the peer's type, which is not observable
    /// from here.
    fn require_decodable_codec(codec: CodecKind) -> Result<()> {
        if matches!(codec, CodecKind::Bincode) {
            bail!(
                "web: sub requires codec=\"json\" (this server is \"bincode\"). \
                 Frames decode into an untyped JSON value, and bincode is not \
                 self-describing, so no frame from any peer could be decoded and \
                 every one would abort the run. Build the server with \
                 WebServer(addr, codec=\"json\")."
            );
        }
        Ok(())
    }

    /// The fallible half of the constructor, kept in `anyhow` so the argument
    /// validation reads the same as every other binding's.
    #[allow(clippy::too_many_arguments)]
    fn build(
        addr: String,
        codec: String,
        static_dir: Option<String>,
        historical: bool,
        cert_path: Option<String>,
        key_path: Option<String>,
        delivery: String,
        lossless_stall_timeout_secs: f64,
    ) -> Result<Self> {
        let mut builder: WebServerBuilder = WebServer::bind(addr)
            .codec(codec_kind(&codec)?)
            .delivery(delivery_kind(&delivery)?)
            .lossless_stall_timeout(lossless_stall_timeout(lossless_stall_timeout_secs)?);
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
    use wingfoil::adapters::web::Envelope;

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
                "auto".into(),
                30.0,
            )
        };
        assert!(build(None, None).is_ok());
        assert!(build(Some("c.pem"), None).is_err());
        assert!(build(None, Some("k.pem")).is_err());
    }

    #[test]
    fn the_stall_timeout_rejects_what_a_duration_cannot_hold() {
        assert_eq!(
            Duration::from_millis(250),
            lossless_stall_timeout(0.25).expect("valid")
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = lossless_stall_timeout(bad).unwrap_err();
            assert!(
                err.to_string()
                    .contains("must be a finite, positive number"),
                "unexpected message for {bad}: {err}"
            );
        }
    }

    #[test]
    fn the_delivery_names_map_and_an_unknown_one_is_rejected() {
        assert!(matches!(delivery_kind("auto"), Ok(Delivery::Auto)));
        assert!(matches!(delivery_kind("lossy"), Ok(Delivery::Lossy)));
        assert!(matches!(delivery_kind("lossless"), Ok(Delivery::Lossless)));
        let err = delivery_kind("best-effort").unwrap_err();
        assert!(
            err.to_string()
                .contains("expected 'auto', 'lossy' or 'lossless'")
        );
    }

    #[test]
    fn a_large_int_falls_back_to_a_float() {
        Python::attach(|py| {
            let value = json(py, "2 ** 70").unwrap();
            assert!(value.as_f64().unwrap() > 1e21);
        });
    }

    // -----------------------------------------------------------------
    // The codec matrix. These pin *why* `sub` rejects bincode while `pub`
    // does not, at the exact call (`CodecKind::encode` / `decode`) the
    // adapter's read.rs and write.rs make. No socket: the whole question is
    // in the codec.
    // -----------------------------------------------------------------

    /// A stand-in for "a typed Rust peer": a plain struct, so decoding it runs
    /// `deserialize_struct` — bare fields in declaration order — exactly as a
    /// user's `web_sub::<SomeStruct>()` does.
    fn peer_struct() -> Envelope {
        Envelope {
            topic: "ui".into(),
            time_ns: 7,
            payload: vec![1, 2],
        }
    }

    /// **The fact the whole rejection rests on.** `sub` decodes into
    /// `serde_json::Value`, whose `Deserialize` asks for `deserialize_any`;
    /// bincode refuses that for *every* input, so a bincode subscription can
    /// never decode a frame — not from a Rust peer, not from another Python
    /// process, not from itself.
    #[test]
    fn bincode_cannot_decode_the_value_that_sub_needs_for_any_shape() {
        for shape in [
            Value::Null,
            Value::Bool(true),
            Value::from(42),
            Value::from(1.5),
            Value::from("hi"),
            serde_json::json!([1, 2]),
            serde_json::json!({"a": 1}),
        ] {
            let bytes = CodecKind::Bincode
                .encode(&shape)
                .expect("bincode encodes a Value fine — it is decoding that cannot work");
            let err = CodecKind::Bincode
                .decode::<Value>(&bytes)
                .expect_err("a bincode Value decode must fail");
            assert!(
                format!("{err:#}").contains("deserialize_any"),
                "unexpected bincode error for {shape}: {err:#}"
            );
        }
    }

    /// Python -> Python under bincode therefore does not round-trip either;
    /// the publisher's own binding cannot read what it wrote.
    #[test]
    fn a_python_peer_cannot_read_a_bincode_python_publish() {
        let published = element_to_json(&PyElement::from(serde_json::json!({"px": 1.5})))
            .expect("a dict marshals");
        let bytes = CodecKind::Bincode.encode(&published).expect("encode");
        assert!(CodecKind::Bincode.decode::<Value>(&bytes).is_err());
    }

    /// So `sub` refuses at wiring rather than once per frame at run time.
    #[test]
    fn sub_refuses_bincode_and_names_the_fix() {
        let err = PyWebServer::require_decodable_codec(CodecKind::Bincode).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("codec=\"json\""), "{message}");
        assert!(message.contains("self-describing"), "{message}");
        assert!(PyWebServer::require_decodable_codec(CodecKind::Json).is_ok());
    }

    /// **Why `pub` is not rejected too.** A scalar or same-width sequence
    /// `Value` encodes byte-for-byte as the matching Rust `T`, so these Rust
    /// peers really do read a bincode Python publish. A blanket rejection
    /// would break them.
    #[test]
    fn a_scalar_python_publish_reaches_a_typed_rust_peer_under_bincode() {
        let float = CodecKind::Bincode
            .encode(&Value::from(1.5))
            .expect("encode");
        assert_eq!(
            1.5,
            CodecKind::Bincode.decode::<f64>(&float).expect("decode")
        );

        let text = CodecKind::Bincode
            .encode(&Value::from("AAPL"))
            .expect("encode");
        assert_eq!(
            "AAPL",
            CodecKind::Bincode.decode::<String>(&text).expect("decode")
        );

        let seq = CodecKind::Bincode
            .encode(&serde_json::json!([1.0, 2.0]))
            .expect("encode");
        assert_eq!(
            vec![1.0, 2.0],
            CodecKind::Bincode.decode::<Vec<f64>>(&seq).expect("decode")
        );
    }

    /// ...but a `dict` against a typed peer is issue #821: a `Value::Object`
    /// encodes as a length-prefixed map with keys inline, while
    /// `deserialize_struct` reads bare fields in declaration order. The
    /// mismatch is not reported — it is silent garbage, which is precisely
    /// what no runtime check here can catch.
    #[test]
    fn a_dict_reaches_a_typed_rust_peer_as_garbage_under_bincode() {
        let peer = peer_struct();
        let as_value = serde_json::to_value(&peer).expect("the struct is JSON-representable");
        let payload = CodecKind::Bincode.encode(&as_value).expect("encode");

        match CodecKind::Bincode.decode::<Envelope>(&payload) {
            Ok(garbage) => assert_ne!(
                garbage, peer,
                "a schema-less payload decoded correctly under bincode: if this \
                 ever holds, revisit the codec table in this module's docs"
            ),
            Err(_) => { /* also fine — either way the value never arrives */ }
        }
    }

    /// The `bytes` mapping is `Vec<u8>`-compatible under **JSON only**: JSON
    /// has one number type, while `Value` writes each element as a bincode
    /// `u64` and a `Vec<u8>` peer reads one byte each.
    #[test]
    fn bytes_reach_a_vec_u8_peer_under_json_but_not_under_bincode() {
        let published = Python::attach(|py| json(py, "bytes([1, 42])").expect("bytes marshal"));

        let json_bytes = CodecKind::Json.encode(&published).expect("encode");
        assert_eq!(
            vec![1_u8, 42],
            CodecKind::Json
                .decode::<Vec<u8>>(&json_bytes)
                .expect("decode")
        );

        let bincode_bytes = CodecKind::Bincode.encode(&published).expect("encode");
        let under_bincode = CodecKind::Bincode.decode::<Vec<u8>>(&bincode_bytes).ok();
        assert_ne!(
            Some(vec![1_u8, 42]),
            under_bincode,
            "if bytes ever survive bincode, revisit the codec table in this module's docs"
        );
    }

    /// The JSON path is what a typed Rust peer and the browser both need, and
    /// it works in both directions for every shape — including the `dict` the
    /// bincode path corrupts.
    #[test]
    fn json_round_trips_both_directions_for_a_typed_peer() {
        let peer = peer_struct();
        // Rust peer -> Python: the struct decodes into the `Value` `sub` uses.
        let from_rust = CodecKind::Json.encode(&peer).expect("encode");
        let seen = CodecKind::Json
            .decode::<Value>(&from_rust)
            .expect("a JSON payload is self-describing");
        assert_eq!(serde_json::json!(7), seen["time_ns"]);

        // Python -> Rust peer: the marshaled dict decodes into the struct.
        let published = element_to_json(&PyElement::from(seen)).expect("a dict marshals");
        let from_python = CodecKind::Json.encode(&published).expect("encode");
        assert_eq!(
            peer,
            CodecKind::Json
                .decode::<Envelope>(&from_python)
                .expect("decode")
        );
    }

    #[test]
    fn an_i64_stays_exact() {
        Python::attach(|py| {
            let element = (-5_i64).into_py_any(py).unwrap();
            assert_eq!(Value::from(-5), to_json(element.bind(py)).unwrap());
        });
    }
}
