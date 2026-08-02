//! Python bindings for the Redis adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;
use crate::types::{DICT_INSERT_INFALLIBLE, LIST_NEW_INFALLIBLE};

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use wingfoil::adapters::redis::{
    RedisConnection, RedisEntry, RedisStreamRecord, redis_pub, redis_stream_read,
    redis_stream_write, redis_sub,
};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Subscribe to a Redis Pub/Sub channel.
///
/// Each tick yields a `list` of event dicts:
/// `{"channel": str, "payload": bytes}`
///
/// Redis Pub/Sub is fire-and-forget: only messages published after the
/// subscription is registered are delivered.
///
/// Args:
///     url: Redis URL, e.g. `"redis://127.0.0.1:6379"`
///     channel: channel to subscribe to
#[pyfunction]
pub fn py_redis_sub(url: String, channel: String) -> PyStream {
    let conn = RedisConnection::new(url);
    let stream = redis_sub(conn, channel);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|event| {
                    let dict = PyDict::new(py);
                    dict.set_item("channel", &event.channel)
                        .expect(DICT_INSERT_INFALLIBLE);
                    dict.set_item("payload", PyBytes::new(py, &event.payload))
                        .expect(DICT_INSERT_INFALLIBLE);
                    dict.into_any().unbind()
                })
                .collect();
            PyElement::new(
                PyList::new(py, items)
                    .expect(LIST_NEW_INFALLIBLE)
                    .into_any()
                    .unbind(),
            )
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.redis_pub()` stream method.
///
/// The stream must yield either:
/// - a single `dict` with `"channel"` (str) and `"payload"` (bytes), or
/// - a `list` of such dicts for multiple messages per tick.
///
/// Dicts that omit `"channel"` fall back to `default_channel`.
pub fn py_redis_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    url: String,
    channel: String,
) -> Rc<dyn Node> {
    let conn = RedisConnection::new(url);
    let default_channel = channel;

    let burst_stream: Rc<dyn Stream<Burst<RedisEntry>>> = stream.map(move |elem| {
        let default_channel = default_channel.clone();
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                std::iter::once(dict_to_entry(dict, &default_channel)).collect()
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| {
                        item.cast_exact::<PyDict>()
                            .ok()
                            .map(|d| dict_to_entry(d, &default_channel))
                    })
                    .collect()
            } else {
                log::error!("redis_pub: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });

    redis_pub(conn, &burst_stream)
}

fn dict_to_entry(dict: &Bound<'_, PyDict>, default_channel: &str) -> RedisEntry {
    let channel = dict
        .get_item("channel")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| default_channel.to_string());
    let payload = dict
        .get_item("payload")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<Vec<u8>>().ok())
        .unwrap_or_default();
    RedisEntry { channel, payload }
}

/// Read a Redis stream: snapshot of existing entries, then live appends.
///
/// Each tick yields a `list` of event dicts:
/// `{"key": str, "id": str, "fields": {name: bytes}}`
///
/// Args:
///     url: Redis URL, e.g. `"redis://127.0.0.1:6379"`
///     key: stream key to read
#[pyfunction]
pub fn py_redis_stream_read(url: String, key: String) -> PyStream {
    let conn = RedisConnection::new(url);
    let stream = redis_stream_read(conn, key);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|event| {
                    let dict = PyDict::new(py);
                    dict.set_item("key", &event.key)
                        .expect(DICT_INSERT_INFALLIBLE);
                    dict.set_item("id", &event.id)
                        .expect(DICT_INSERT_INFALLIBLE);
                    let fields = PyDict::new(py);
                    for (name, value) in &event.fields {
                        fields
                            .set_item(name, PyBytes::new(py, value))
                            .expect(DICT_INSERT_INFALLIBLE);
                    }
                    dict.set_item("fields", fields)
                        .expect(DICT_INSERT_INFALLIBLE);
                    dict.into_any().unbind()
                })
                .collect();
            PyElement::new(
                PyList::new(py, items)
                    .expect(LIST_NEW_INFALLIBLE)
                    .into_any()
                    .unbind(),
            )
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.redis_stream_write()` stream method.
///
/// The stream must yield either:
/// - a single `dict` with optional `"key"` (str) and `"fields"` (dict of `name -> bytes`), or
/// - a `list` of such dicts for multiple entries per tick.
///
/// Dicts that omit `"key"` fall back to `default_key`.
pub fn py_redis_stream_write_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    url: String,
    key: String,
) -> Rc<dyn Node> {
    let conn = RedisConnection::new(url);
    let default_key = key;

    let burst_stream: Rc<dyn Stream<Burst<RedisStreamRecord>>> = stream.map(move |elem| {
        let default_key = default_key.clone();
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                std::iter::once(dict_to_record(dict, &default_key)).collect()
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| {
                        item.cast_exact::<PyDict>()
                            .ok()
                            .map(|d| dict_to_record(d, &default_key))
                    })
                    .collect()
            } else {
                log::error!("redis_stream_write: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });

    redis_stream_write(conn, &burst_stream)
}

fn dict_to_record(dict: &Bound<'_, PyDict>, default_key: &str) -> RedisStreamRecord {
    let key = dict
        .get_item("key")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| default_key.to_string());
    let fields = dict
        .get_item("fields")
        .ok()
        .flatten()
        .and_then(|v| v.cast_into::<PyDict>().ok())
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(name, value)| {
                    let name = name.extract::<String>().ok()?;
                    let value = value.extract::<Vec<u8>>().ok()?;
                    Some((name, value))
                })
                .collect()
        })
        .unwrap_or_default();
    RedisStreamRecord { key, fields }
}
