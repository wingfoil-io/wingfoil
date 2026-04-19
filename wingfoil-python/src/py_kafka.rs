//! Python bindings for the Kafka adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use wingfoil::adapters::kafka::{KafkaConnection, KafkaRecord, kafka_pub, kafka_sub};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Subscribe to a Kafka topic.
///
/// Each tick yields a `list` of event dicts:
/// `{"topic": str, "partition": int, "offset": int, "key": bytes | None, "value": bytes}`
///
/// Args:
///     brokers: Kafka bootstrap servers, e.g. `"localhost:9092"`
///     topic: Topic to subscribe to
///     group_id: Consumer group ID
#[pyfunction]
pub fn py_kafka_sub(brokers: String, topic: String, group_id: String) -> PyStream {
    let conn = KafkaConnection::new(brokers);
    let stream = kafka_sub(conn, topic, group_id);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|event| {
                    let dict = PyDict::new(py);
                    dict.set_item("topic", &event.topic).unwrap();
                    dict.set_item("partition", event.partition).unwrap();
                    dict.set_item("offset", event.offset).unwrap();
                    match &event.key {
                        Some(k) => {
                            dict.set_item("key", PyBytes::new(py, k)).unwrap();
                        }
                        None => {
                            dict.set_item("key", py.None()).unwrap();
                        }
                    }
                    dict.set_item("value", PyBytes::new(py, &event.value))
                        .unwrap();
                    dict.into_any().unbind()
                })
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.kafka_pub()` stream method.
///
/// The stream must yield either:
/// - a single `dict` with `"topic"` (str), `"value"` (bytes), and optionally `"key"` (bytes), or
/// - a `list` of such dicts for multiple writes per tick.
pub fn py_kafka_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    brokers: String,
    topic: String,
) -> Rc<dyn Node> {
    let conn = KafkaConnection::new(brokers);
    let default_topic = topic;

    let burst_stream: Rc<dyn Stream<Burst<KafkaRecord>>> = stream.map(move |elem| {
        let default_topic = default_topic.clone();
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                std::iter::once(dict_to_record(dict, &default_topic)).collect()
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| {
                        item.cast_exact::<PyDict>()
                            .ok()
                            .map(|d| dict_to_record(d, &default_topic))
                    })
                    .collect()
            } else {
                log::error!("kafka_pub: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });

    kafka_pub(conn, &burst_stream)
}

fn dict_to_record(dict: &Bound<'_, PyDict>, default_topic: &str) -> KafkaRecord {
    let topic = dict
        .get_item("topic")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| default_topic.to_string());
    let key = dict
        .get_item("key")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<Vec<u8>>().ok());
    let value = dict
        .get_item("value")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<Vec<u8>>().ok())
        .unwrap_or_default();
    KafkaRecord { topic, key, value }
}
