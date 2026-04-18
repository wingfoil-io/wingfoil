//! Python bindings for the Fluvio adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use wingfoil::adapters::fluvio::{FluvioConnection, FluvioRecord, fluvio_pub, fluvio_sub};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Subscribe to a Fluvio topic partition.
///
/// Streams records continuously from the given partition, starting at `start_offset`
/// (or from the beginning if `None`). Each tick yields a `list` of event dicts:
///
/// ```python
/// {"key": bytes | None, "value": bytes, "offset": int}
/// ```
///
/// Args:
///     endpoint: Fluvio SC endpoint, e.g. `"127.0.0.1:9003"`
///     topic: Fluvio topic name
///     partition: Partition index (0-based)
///     start_offset: Absolute offset to start consuming from.
///                   Pass `None` to start from the beginning (default).
#[pyfunction]
#[pyo3(signature = (endpoint, topic, partition=0, start_offset=None))]
pub fn py_fluvio_sub(
    endpoint: String,
    topic: String,
    partition: u32,
    start_offset: Option<i64>,
) -> PyStream {
    let conn = FluvioConnection::new(endpoint);
    let stream = fluvio_sub(conn, topic, partition, start_offset);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|event| {
                    let dict = PyDict::new(py);
                    match &event.key {
                        Some(k) => dict.set_item("key", PyBytes::new(py, k)).unwrap(),
                        None => dict.set_item("key", py.None()).unwrap(),
                    }
                    dict.set_item("value", PyBytes::new(py, &event.value))
                        .unwrap();
                    dict.set_item("offset", event.offset).unwrap();
                    dict.into_any().unbind()
                })
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.fluvio_pub()` stream method.
///
/// The stream must yield either:
/// - a single `dict` with `"value"` (bytes) and optional `"key"` (str), or
/// - a `list` of such dicts for multiple records per tick.
pub fn py_fluvio_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    endpoint: String,
    topic: String,
) -> Rc<dyn Node> {
    let conn = FluvioConnection::new(endpoint);

    let burst_stream: Rc<dyn Stream<Burst<FluvioRecord>>> = stream.map(move |elem| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                std::iter::once(dict_to_record(dict)).collect()
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| item.cast_exact::<PyDict>().ok().map(|d| dict_to_record(d)))
                    .collect()
            } else {
                log::error!("fluvio_pub: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });

    fluvio_pub(conn, topic, &burst_stream)
}

fn dict_to_record(dict: &Bound<'_, PyDict>) -> FluvioRecord {
    let key = dict
        .get_item("key")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok());
    let value = dict
        .get_item("value")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<Vec<u8>>().ok())
        .unwrap_or_default();
    FluvioRecord { key, value }
}
