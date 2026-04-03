//! Python bindings for the etcd adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use std::time::Duration;
use wingfoil::adapters::etcd::{EtcdConnection, EtcdEntry, EtcdEventKind, etcd_pub, etcd_sub};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Subscribe to etcd keys matching a prefix.
///
/// Emits a snapshot of all existing keys under the prefix, then delivers live
/// watch events as they arrive. Each tick yields a `list` of event dicts:
///
/// ```python
/// {"kind": "put" | "delete", "key": str, "value": bytes, "revision": int}
/// ```
///
/// Args:
///     endpoint: etcd endpoint, e.g. `"http://localhost:2379"`
///     prefix: key prefix to watch, e.g. `"/myapp/"`
#[pyfunction]
pub fn py_etcd_sub(endpoint: String, prefix: String) -> PyStream {
    let conn = EtcdConnection::new(endpoint);
    let stream = etcd_sub(conn, prefix);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|event| {
                    let dict = PyDict::new(py);
                    let kind_str = match event.kind {
                        EtcdEventKind::Put => "put",
                        EtcdEventKind::Delete => "delete",
                    };
                    dict.set_item("kind", kind_str).unwrap();
                    dict.set_item("key", &event.entry.key).unwrap();
                    dict.set_item("value", PyBytes::new(py, &event.entry.value))
                        .unwrap();
                    dict.set_item("revision", event.revision).unwrap();
                    dict.into_any().unbind()
                })
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.etcd_pub()` stream method.
///
/// The stream must yield either:
/// - a single `dict` with `"key"` (str) and `"value"` (bytes), or
/// - a `list` of such dicts for multiple writes per tick.
pub fn py_etcd_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    endpoint: String,
    lease_ttl: Option<f64>,
    force: bool,
) -> Rc<dyn Node> {
    let conn = EtcdConnection::new(endpoint);
    let lease_duration = lease_ttl.map(Duration::from_secs_f64);

    let burst_stream: Rc<dyn Stream<Burst<EtcdEntry>>> = stream.map(move |elem| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                std::iter::once(dict_to_entry(dict)).collect()
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| item.cast_exact::<PyDict>().ok().map(|d| dict_to_entry(d)))
                    .collect()
            } else {
                log::error!("etcd_pub: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });

    etcd_pub(conn, &burst_stream, lease_duration, force)
}

fn dict_to_entry(dict: &Bound<'_, PyDict>) -> EtcdEntry {
    let key = dict
        .get_item("key")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_default();
    let value = dict
        .get_item("value")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<Vec<u8>>().ok())
        .unwrap_or_default();
    EtcdEntry { key, value }
}
