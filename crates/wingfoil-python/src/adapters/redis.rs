//! Python bindings for the wingfoil **Redis** adapter
//! ([`wingfoil::adapters::redis`]).
//!
//! Four graph entry points, two per Redis surface, all `#[pyadapter]`-generated:
//!
//! | Python                         | Rust                    | shape |
//! |--------------------------------|-------------------------|-------|
//! | `redis_sub(graph, …)`          | [`redis_sub`]           | Pub/Sub subscriber |
//! | `redis_pub(stream, …)`         | [`RedisSinkOps`]        | Pub/Sub `PUBLISH` sink |
//! | `redis_stream_read(graph, …)`  | [`redis_stream_read`]   | stream snapshot + live tail |
//! | `redis_stream_write(stream, …)`| [`RedisStreamSinkOps`]  | stream `XADD` sink |
//!
//! # The dynamic edge
//!
//! Both record types are concrete on the Rust side, so — as with kafka — only
//! the *shape* is dynamic:
//!
//! - **reads** erase [`RedisEvent`] to `{"channel": str, "payload": bytes}` and
//!   [`RedisStreamEvent`] to `{"key": str, "id": str, "fields": {str: bytes}}`,
//!   both through plain `From<…> for PyElement`, so the sources go through the
//!   standard `erase_burst_source` seam with no intermediate type;
//! - **writes** marshal a Python `dict` into a [`RedisEntry`] /
//!   [`RedisStreamRecord`] via [`RecordDict`]. Both need their `channel`/`key`
//!   fallback argument, which is not in scope at the `typed_burst_input` seam,
//!   so the sinks keep their input **erased** and marshal inside the fn.
//!
//! Every source is burst-shaped, so each tick yields a Python **`list`** of
//! event dicts — the messages that arrived between cycles, losslessly.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Marshaling fails loudly.** Legacy's `dict_to_entry` / `dict_to_record`
//!    defaulted through every error: a missing or wrong-typed `payload` became
//!    empty bytes, a `fields` entry that was not `str -> bytes` was silently
//!    dropped from the record, a non-dict `fields` became no fields at all, and
//!    a non-dict stream value logged and produced an *empty burst* — writing
//!    nothing. Each of those now aborts the run naming the offending field.
//! 2. **The run mode is an argument.** Both sources reject a historical run at
//!    wiring (Pub/Sub and `XREAD BLOCK 0` are live, unbounded streams with no
//!    timeline to replay), and a Python `Graph` does not know its mode until
//!    `run()` — so `realtime` is explicit and must match the eventual
//!    `graph.run(realtime=…)`. `realtime=False` raises here.
//! 3. **`channel` / `key` are optional on the sinks.** Legacy took a required
//!    default; a record names its own target, so one sink can write to many.
//!    The argument supplies the fallback for dicts that omit it, and a dict with
//!    neither is an error rather than a write to `""`.
//! 4. **New capability: `buffer_size`.** Both sinks take the engine adapter's
//!    back-pressure bound (`None` = unbounded), which legacy had no equivalent
//!    for.

use anyhow::Result;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use wingfoil::adapters::redis::{
    RedisConnection, RedisEntry, RedisEvent, RedisSinkOps, RedisStreamEvent, RedisStreamRecord,
    RedisStreamSinkOps, redis_stream_read as redis_stream_read_source,
    redis_sub as redis_subscribe,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::{RecordDict, record_dict, run_mode};
use crate::{PyElement, pyadapter};

/// Error-message prefixes for the two sinks' marshaling failures.
const PUB: &str = "redis_pub";
const STREAM_WRITE: &str = "redis_stream_write";

/// Insert into a fresh dict, where the only documented failure is unreachable.
fn set(dict: &Bound<'_, PyDict>, key: &str, value: Bound<'_, PyAny>) {
    dict.set_item(key, value)
        .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
}

/// Box a `&str` for insertion. Primitive conversions are infallible.
fn py_str<'py>(py: Python<'py>, value: &str) -> Bound<'py, PyAny> {
    value
        .into_pyobject(py)
        .expect("invariant: scalar -> PyObject conversion is infallible")
        .into_any()
}

// ---------------------------------------------------------------------------
// Reading: events -> Python dicts.
// ---------------------------------------------------------------------------

/// A Pub/Sub message erases to `{"channel": str, "payload": bytes}`. The payload
/// stays `bytes` — Redis payloads are opaque, so this does not guess at UTF-8.
impl From<RedisEvent> for PyElement {
    fn from(event: RedisEvent) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            set(&dict, "channel", py_str(py, &event.channel));
            set(
                &dict,
                "payload",
                PyBytes::new(py, &event.payload).into_any(),
            );
            PyElement::new(dict.into_any().unbind())
        })
    }
}

/// A stream entry erases to `{"key": str, "id": str, "fields": {str: bytes}}`.
/// `fields` is a nested dict; Redis preserves the entry's field order, and so
/// does the Python dict built here.
impl From<RedisStreamEvent> for PyElement {
    fn from(event: RedisStreamEvent) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            set(&dict, "key", py_str(py, &event.key));
            set(&dict, "id", py_str(py, &event.id));
            let fields = PyDict::new(py);
            for (name, value) in &event.fields {
                set(&fields, name, PyBytes::new(py, value).into_any());
            }
            set(&dict, "fields", fields.into_any());
            PyElement::new(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Writing: Python dicts -> records.
// ---------------------------------------------------------------------------

/// Marshal one erased stream value into a Pub/Sub entry.
fn element_to_entry(elem: &PyElement, default_channel: Option<&str>) -> Result<RedisEntry> {
    Python::attach(|py| {
        let dict = record_dict(elem, py, PUB, "with 'payload' (and optionally 'channel')")?;
        let record = RecordDict::new(&dict, PUB);
        Ok(RedisEntry {
            channel: record.str_or("channel", default_channel, "channel")?,
            payload: record.bytes("payload")?,
        })
    })
}

/// Marshal one erased stream value into a stream record.
fn element_to_stream_record(
    elem: &PyElement,
    default_key: Option<&str>,
) -> Result<RedisStreamRecord> {
    Python::attach(|py| {
        let dict = record_dict(
            elem,
            py,
            STREAM_WRITE,
            "with 'fields' (and optionally 'key')",
        )?;
        let record = RecordDict::new(&dict, STREAM_WRITE);
        Ok(RedisStreamRecord {
            key: record.str_or("key", default_key, "key")?,
            fields: record.byte_fields("fields")?,
        })
    })
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Subscribe to a Redis Pub/Sub channel.
///
/// Each tick yields a `list` of `{"channel": str, "payload": bytes}` dicts — the
/// messages that arrived between graph cycles. Call `[0]` for the single-message
/// case.
///
/// Pub/Sub is fire-and-forget: only messages published **after** the
/// subscription is registered are delivered, and nothing is replayed.
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. Pub/Sub has no historical timeline, so
/// `realtime=False` raises here.
#[pyadapter(name = redis_sub, source)]
#[pyo3(signature = (url, channel, realtime = true))]
fn sub(
    g: &GraphBuilder,
    url: String,
    channel: String,
    realtime: bool,
) -> Result<Stream<Burst<RedisEvent>>> {
    redis_subscribe(g, run_mode(realtime), RedisConnection::new(url), channel)
}

/// Publish this stream to Redis Pub/Sub via `PUBLISH`.
///
/// Each tick's value is either a single `dict` or a `list`/`tuple` of dicts for
/// several messages at one timestamp. A message dict is
/// `{"payload": bytes, "channel": str (optional)}`.
///
/// Each entry names its own target channel, so one sink can publish to many;
/// `channel` supplies the default for dicts that omit it. A dict with neither, a
/// missing `"payload"`, a wrong-typed field, or a non-dict stream value aborts
/// the run rather than publishing a malformed message.
///
/// `buffer_size` bounds the unwritten backlog (`None` = unbounded). The
/// connection opens lazily on the first write, so a connection failure aborts
/// the run rather than raising at wiring; a malformed URL still raises here.
/// Returns a terminal stream whose value is `None`.
#[pyadapter(name = redis_pub)]
#[pyo3(signature = (url, channel = None, buffer_size = None))]
fn publish(
    stream: &Stream<Burst<PyElement>>,
    url: String,
    channel: Option<String>,
    buffer_size: Option<usize>,
) -> Result<Stream<()>> {
    let entries: Stream<Burst<RedisEntry>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(|elem| element_to_entry(elem, channel.as_deref()))
            .collect::<Result<Burst<RedisEntry>>>()
    });
    entries.redis_pub(RedisConnection::new(url), buffer_size)
}

/// Read a Redis stream: a snapshot of the existing entries, then the live tail.
///
/// Each tick yields a `list` of
/// `{"key": str, "id": str, "fields": {str: bytes}}` dicts. `id` is the
/// Redis-assigned entry ID (e.g. `"1526919030474-0"`).
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. The tail (`XREAD BLOCK 0`) is live and
/// unbounded, so `realtime=False` raises here.
#[pyadapter(name = redis_stream_read, source)]
#[pyo3(signature = (url, key, realtime = true))]
fn stream_read(
    g: &GraphBuilder,
    url: String,
    key: String,
    realtime: bool,
) -> Result<Stream<Burst<RedisStreamEvent>>> {
    redis_stream_read_source(g, run_mode(realtime), RedisConnection::new(url), key)
}

/// Append this stream to a Redis stream via `XADD`.
///
/// Each tick's value is either a single `dict` or a `list`/`tuple` of dicts for
/// several entries at one timestamp. An entry dict is
/// `{"fields": {str: bytes}, "key": str (optional)}`; Redis assigns the entry ID
/// (`XADD <key> *`), so it is not part of the record.
///
/// Each record names its own target stream, so one sink can append to many;
/// `key` supplies the default for dicts that omit it. A dict with neither, a
/// missing or non-dict `"fields"`, a field value that is not bytes-like, or a
/// non-dict stream value aborts the run rather than appending a partial entry.
///
/// `buffer_size` bounds the unwritten backlog (`None` = unbounded). The
/// connection opens lazily on the first write, so a connection failure aborts
/// the run rather than raising at wiring; a malformed URL still raises here.
/// Returns a terminal stream whose value is `None`.
#[pyadapter(name = redis_stream_write)]
#[pyo3(signature = (url, key = None, buffer_size = None))]
fn stream_write(
    stream: &Stream<Burst<PyElement>>,
    url: String,
    key: Option<String>,
    buffer_size: Option<usize>,
) -> Result<Stream<()>> {
    let records: Stream<Burst<RedisStreamRecord>> =
        stream.try_map(move |burst: &Burst<PyElement>| {
            burst
                .iter()
                .map(|elem| element_to_stream_record(elem, key.as_deref()))
                .collect::<Result<Burst<RedisStreamRecord>>>()
        });
    records.redis_stream_write(RedisConnection::new(url), buffer_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyList;

    fn dict_element(py: Python<'_>, items: &[(&str, Py<PyAny>)]) -> PyElement {
        let dict = PyDict::new(py);
        for (k, v) in items {
            dict.set_item(k, v).unwrap();
        }
        PyElement::new(dict.into_any().unbind())
    }

    fn bytes_obj(py: Python<'_>, b: &[u8]) -> Py<PyAny> {
        PyBytes::new(py, b).into_any().unbind()
    }

    fn str_obj(py: Python<'_>, s: &str) -> Py<PyAny> {
        s.into_pyobject(py).unwrap().into_any().unbind()
    }

    fn fields_obj(py: Python<'_>, items: &[(&str, &[u8])]) -> Py<PyAny> {
        let d = PyDict::new(py);
        for (k, v) in items {
            d.set_item(k, PyBytes::new(py, v)).unwrap();
        }
        d.into_any().unbind()
    }

    #[test]
    fn a_pubsub_event_erases_to_a_dict() {
        let element = PyElement::from(RedisEvent {
            channel: "quotes".into(),
            payload: b"tick".to_vec(),
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("quotes", get("channel").extract::<String>().unwrap());
            assert_eq!(
                b"tick".to_vec(),
                get("payload").extract::<Vec<u8>>().unwrap()
            );
            assert!(get("payload").is_instance_of::<PyBytes>());
        });
    }

    #[test]
    fn a_stream_event_erases_with_a_nested_fields_dict_in_order() {
        let element = PyElement::from(RedisStreamEvent {
            key: "trades".into(),
            id: "1526919030474-0".into(),
            fields: vec![
                ("sym".into(), b"AAPL".to_vec()),
                ("px".into(), b"1.5".to_vec()),
            ],
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("trades", get("key").extract::<String>().unwrap());
            assert_eq!("1526919030474-0", get("id").extract::<String>().unwrap());
            let fields = get("fields").cast_into::<PyDict>().unwrap();
            let names: Vec<String> = fields
                .keys()
                .iter()
                .map(|k| k.extract::<String>().unwrap())
                .collect();
            assert_eq!(vec!["sym".to_string(), "px".to_string()], names);
            assert_eq!(
                b"AAPL".to_vec(),
                fields
                    .get_item("sym")
                    .unwrap()
                    .unwrap()
                    .extract::<Vec<u8>>()
                    .unwrap()
            );
        });
    }

    #[test]
    fn an_entry_with_no_fields_erases_to_an_empty_dict() {
        let element = PyElement::from(RedisStreamEvent::default());
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let fields = dict
                .get_item("fields")
                .unwrap()
                .unwrap()
                .cast_into::<PyDict>()
                .unwrap();
            assert_eq!(0, fields.len());
        });
    }

    #[test]
    fn a_dict_marshals_to_a_pubsub_entry() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("channel", str_obj(py, "quotes")),
                    ("payload", bytes_obj(py, b"tick")),
                ],
            );
            let entry = element_to_entry(&element, None).unwrap();
            assert_eq!("quotes", entry.channel);
            assert_eq!(b"tick".to_vec(), entry.payload);
        });
    }

    #[test]
    fn the_default_channel_fills_in_and_the_dict_wins_over_it() {
        Python::attach(|py| {
            let bare = dict_element(py, &[("payload", bytes_obj(py, b"p"))]);
            assert_eq!("fb", element_to_entry(&bare, Some("fb")).unwrap().channel);

            let named = dict_element(
                py,
                &[
                    ("channel", str_obj(py, "own")),
                    ("payload", bytes_obj(py, b"p")),
                ],
            );
            assert_eq!("own", element_to_entry(&named, Some("fb")).unwrap().channel);
        });
    }

    #[test]
    fn no_channel_and_no_default_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("payload", bytes_obj(py, b"p"))]);
            let err = element_to_entry(&element, None).unwrap_err();
            assert!(err.to_string().contains("no 'channel'"), "got: {err}");
        });
    }

    #[test]
    fn a_missing_payload_errors_rather_than_publishing_empty_bytes() {
        Python::attach(|py| {
            let element = dict_element(py, &[("channel", str_obj(py, "c"))]);
            let err = element_to_entry(&element, None).unwrap_err();
            assert!(err.to_string().contains("missing 'payload'"), "got: {err}");
        });
    }

    #[test]
    fn a_wrong_typed_payload_errors() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("channel", str_obj(py, "c")),
                    (
                        "payload",
                        1.5_f64.into_pyobject(py).unwrap().into_any().unbind(),
                    ),
                ],
            );
            assert!(element_to_entry(&element, None).is_err());
        });
    }

    #[test]
    fn a_non_dict_value_errors_rather_than_writing_nothing() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_entry(&element, None).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "got: {err}");
        });
    }

    #[test]
    fn an_empty_element_errors() {
        let err = element_to_entry(&PyElement::none(), None).unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    #[test]
    fn a_dict_marshals_to_a_stream_record() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("key", str_obj(py, "trades")),
                    (
                        "fields",
                        fields_obj(py, &[("sym", b"AAPL"), ("px", b"1.5")]),
                    ),
                ],
            );
            let record = element_to_stream_record(&element, None).unwrap();
            assert_eq!("trades", record.key);
            assert_eq!(
                vec![
                    ("sym".to_string(), b"AAPL".to_vec()),
                    ("px".to_string(), b"1.5".to_vec())
                ],
                record.fields
            );
        });
    }

    #[test]
    fn the_default_stream_key_fills_in() {
        Python::attach(|py| {
            let element = dict_element(py, &[("fields", fields_obj(py, &[("a", b"1")]))]);
            let record = element_to_stream_record(&element, Some("fb")).unwrap();
            assert_eq!("fb", record.key);
        });
    }

    #[test]
    fn missing_fields_errors_rather_than_appending_an_empty_entry() {
        Python::attach(|py| {
            let element = dict_element(py, &[("key", str_obj(py, "k"))]);
            let err = element_to_stream_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("missing 'fields'"), "got: {err}");
        });
    }

    #[test]
    fn a_non_dict_fields_errors_rather_than_dropping_every_field() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[("key", str_obj(py, "k")), ("fields", str_obj(py, "nope"))],
            );
            let err = element_to_stream_record(&element, None).unwrap_err();
            assert!(
                err.to_string().contains("must be a dict of str -> bytes"),
                "got: {err}"
            );
        });
    }

    #[test]
    fn a_wrong_typed_field_value_errors_rather_than_being_dropped() {
        Python::attach(|py| {
            let inner = PyDict::new(py);
            inner.set_item("sym", 7_i64).unwrap();
            let element = dict_element(
                py,
                &[
                    ("key", str_obj(py, "k")),
                    ("fields", inner.into_any().unbind()),
                ],
            );
            let err = element_to_stream_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("for 'sym'"), "got: {err}");
        });
    }
}
