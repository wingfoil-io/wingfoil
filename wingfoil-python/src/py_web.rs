//! Python bindings for the `web` adapter.
//!
//! Python streams work with native dicts / lists / bytes / primitives.
//! The binding marshals them to / from `serde_json::Value` which the
//! wingfoil web adapter can serialise with either the `bincode` or
//! `json` codec. The wingfoil-wasm browser client decodes the bytes
//! the same way.

use std::rc::Rc;

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyNone, PyString};
use wingfoil::adapters::web::{CodecKind, WebServer, WebServerBuilder, web_pub, web_sub};
use wingfoil::{Burst, Node, Stream, StreamOperators};

use crate::PyNode;
use crate::py_element::PyElement;
use crate::py_stream::PyStream;

/// Python-facing handle to a running [`WebServer`].
///
/// Mirrors the builder on the Rust side: construct with a bind address,
/// optionally set the codec and static-file directory, then register
/// publish / subscribe streams.
#[pyclass(unsendable, name = "WebServer")]
pub struct PyWebServer {
    inner: WebServer,
}

impl PyWebServer {
    /// Access the underlying Rust [`WebServer`]. Used by stream
    /// methods that wire `web_pub` into the graph.
    pub(crate) fn inner_ref(&self) -> &WebServer {
        &self.inner
    }
}

#[pymethods]
impl PyWebServer {
    /// Bind and start an HTTP + WebSocket server.
    ///
    /// Args:
    ///     addr: e.g. "127.0.0.1:0" or "0.0.0.0:8080"
    ///     codec: "bincode" (default) or "json"
    ///     static_dir: optional path to serve under GET /
    ///     historical: if True, returns a no-op server suitable for
    ///         RunMode::HistoricalFrom runs (no TCP port bound)
    #[new]
    #[pyo3(signature = (addr, codec=None, static_dir=None, historical=false))]
    fn new(
        addr: String,
        codec: Option<String>,
        static_dir: Option<String>,
        historical: bool,
    ) -> PyResult<Self> {
        let mut builder: WebServerBuilder = WebServer::bind(addr);
        if let Some(c) = codec {
            let kind = match c.as_str() {
                "bincode" => CodecKind::Bincode,
                "json" => CodecKind::Json,
                other => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "unknown codec '{other}' (expected 'bincode' or 'json')"
                    )));
                }
            };
            builder = builder.codec(kind);
        }
        if let Some(dir) = static_dir {
            builder = builder.serve_static(dir);
        }
        let inner = if historical {
            builder.start_historical()
        } else {
            builder.start()
        }
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self { inner })
    }

    /// The bound port (0 when `historical=True`).
    fn port(&self) -> u16 {
        self.inner.port()
    }

    /// Return the codec currently used on the wire ("bincode" or "json").
    fn codec_name(&self) -> &'static str {
        match self.inner.codec().kind() {
            CodecKind::Bincode => "bincode",
            CodecKind::Json => "json",
        }
    }

    /// Publish a stream on `topic`. Returns a sink Node.
    ///
    /// Stream values must be JSON-compatible Python objects (dict /
    /// list / str / int / float / bool / bytes / None). Values that
    /// cannot be converted are published as JSON null and logged.
    fn pub_(&self, topic: String, stream: &PyStream) -> PyNode {
        PyNode::new(py_web_pub_inner(&stream.0, &self.inner, topic))
    }

    /// Subscribe to `topic`. Returns a source Stream of dicts / lists /
    /// primitives decoded from the clients' frames.
    fn sub(&self, topic: String) -> PyStream {
        py_web_sub(&self.inner, topic)
    }
}

/// Rust inner impl for the `.web_pub(server, topic)` fluent method on
/// `PyStream`. See also [`PyStream::web_pub`] in `py_stream.rs`.
pub fn py_web_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    server: &WebServer,
    topic: String,
) -> Rc<dyn Node> {
    let value_stream: Rc<dyn Stream<serde_json::Value>> = stream.map(|elem| {
        Python::attach(|py| py_to_serde(elem.as_ref().bind(py)).unwrap_or(serde_json::Value::Null))
    });
    web_pub(server, topic, &value_stream)
}

/// Subscribe to `topic` and emit `Burst<PyElement>` (list of dicts /
/// primitives) each tick.
pub fn py_web_sub(server: &WebServer, topic: String) -> PyStream {
    let burst_stream: Rc<dyn Stream<Burst<serde_json::Value>>> = web_sub(server, topic);
    let py_stream = burst_stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|v| serde_to_py(py, &v).unbind())
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });
    PyStream(py_stream)
}

// ---- conversions ----

fn py_to_serde(obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if obj.is_instance_of::<PyNone>() {
        return Ok(serde_json::Value::Null);
    }
    if obj.is_instance_of::<PyBool>() {
        return Ok(serde_json::Value::Bool(obj.extract::<bool>()?));
    }
    if obj.is_instance_of::<PyInt>() {
        if let Ok(v) = obj.extract::<i64>() {
            return Ok(serde_json::Value::Number(v.into()));
        }
        if let Ok(v) = obj.extract::<u64>() {
            return Ok(serde_json::Value::Number(v.into()));
        }
        // Fallback to float for very large ints.
        if let Ok(v) = obj.extract::<f64>() {
            return serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("web_pub: non-finite float")
                });
        }
    }
    if obj.is_instance_of::<PyFloat>() {
        let v = obj.extract::<f64>()?;
        return serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("web_pub: non-finite float"));
    }
    if obj.is_instance_of::<PyString>() {
        return Ok(serde_json::Value::String(obj.extract::<String>()?));
    }
    if let Ok(bytes) = obj.cast::<PyBytes>() {
        // Encode bytes as a JSON array of u8 for maximum compatibility.
        let data = bytes.as_bytes();
        let arr: Vec<serde_json::Value> = data
            .iter()
            .map(|b| serde_json::Value::Number((*b as u64).into()))
            .collect();
        return Ok(serde_json::Value::Array(arr));
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let mut out = Vec::with_capacity(list.len());
        for item in list.iter() {
            out.push(py_to_serde(&item)?);
        }
        return Ok(serde_json::Value::Array(out));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let key = k.extract::<String>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "web_pub: dict keys must be strings for JSON-compatible publish",
                )
            })?;
            map.insert(key, py_to_serde(&v)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "web_pub: unsupported value type: {}",
        obj.get_type().name()?
    )))
}

fn serde_to_py<'py>(py: Python<'py>, value: &serde_json::Value) -> Bound<'py, PyAny> {
    use serde_json::Value as V;
    match value {
        V::Null => py.None().into_bound(py),
        V::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        V::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py).unwrap().into_any()
            } else if let Some(u) = n.as_u64() {
                u.into_pyobject(py).unwrap().into_any()
            } else if let Some(f) = n.as_f64() {
                f.into_pyobject(py).unwrap().into_any()
            } else {
                py.None().into_bound(py)
            }
        }
        V::String(s) => s.as_str().into_pyobject(py).unwrap().into_any(),
        V::Array(arr) => {
            let items: Vec<Bound<'py, PyAny>> = arr.iter().map(|v| serde_to_py(py, v)).collect();
            PyList::new(py, items).unwrap().into_any()
        }
        V::Object(obj) => {
            let d = PyDict::new(py);
            for (k, v) in obj.iter() {
                d.set_item(k, serde_to_py(py, v)).unwrap();
            }
            d.into_any()
        }
    }
}
