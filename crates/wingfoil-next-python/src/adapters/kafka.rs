//! Python bindings for the wingfoil-next **Kafka** adapter
//! ([`wingfoil_next::adapters::kafka`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                    | Rust               | shape |
//! |---------------------------|--------------------|-------|
//! | `kafka_sub(graph, …)`     | [`kafka_sub`]      | real-time consumer, one `Burst<KafkaEvent>` per cycle |
//! | `kafka_pub(stream, …)`    | [`KafkaSinkOps`]   | producer sink |
//!
//! # The dynamic edge
//!
//! The Rust adapter carries two concrete record types, so — unlike postgres —
//! only the *shape* is dynamic, not the schema:
//!
//! - **reads** decode [`KafkaEvent`] into a Python **`dict`**
//!   (`topic` / `partition` / `offset` / `key` / `value`) via
//!   `From<KafkaEvent> for PyElement`, so the source goes through the standard
//!   `erase_burst_source` seam with no intermediate type;
//! - **writes** marshal a Python `dict` into a [`KafkaRecord`]. That direction
//!   needs the `topic` fallback argument, which is not in scope at the
//!   `typed_burst_input` seam, so the sink keeps its input **erased**
//!   (`Stream<Burst<PyElement>>`) and marshals inside the fn — the postgres
//!   pattern.
//!
//! The source is burst-shaped (`Stream<Burst<KafkaEvent>>`), so each tick yields
//! a Python **`list` of event dicts** — the messages that arrived between
//! cycles, losslessly.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Marshaling fails loudly.** Legacy `dict_to_record` defaulted its way
//!    through every error: a missing `value` produced empty bytes, a wrong-typed
//!    `key` was dropped, and a non-dict stream value logged an error and
//!    produced an *empty burst* — publishing nothing, silently. Here a missing
//!    `value`, a wrong-typed field, or a non-dict value aborts the run naming
//!    the offending field.
//! 2. **The run mode is an argument.** `kafka_sub` rejects a historical run at
//!    wiring (a live consumer has no timeline to replay), and a Python `Graph`
//!    does not know its mode until `run()` — so `realtime` is passed explicitly
//!    and must match the eventual `graph.run(realtime=…)`. `realtime=False`
//!    raises here.
//! 3. **`topic` is optional on the sink.** Legacy took a required default topic;
//!    a [`KafkaRecord`] names its own target, so one sink can write to many
//!    topics. Pass `topic` to supply the default for dicts that omit it — a dict
//!    with neither is an error rather than a publish to `""`.

use anyhow::Result;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use wingfoil_next::adapters::kafka::{
    KafkaConnection, KafkaEvent, KafkaRecord, KafkaSinkOps, kafka_sub as kafka_subscribe,
};
use wingfoil_next::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::{RecordDict, record_dict, run_mode};
use crate::{PyElement, pyadapter};

/// The error-message prefix for the sink's marshaling failures.
const WHO: &str = "kafka_pub";

// ---------------------------------------------------------------------------
// Reading: an event -> a Python dict.
// ---------------------------------------------------------------------------

/// A consumed message erases to a `dict`. `key` is `bytes` or `None`; `value` is
/// always `bytes` (Kafka payloads are opaque — decoding is the caller's call, so
/// this does not guess at UTF-8).
impl From<KafkaEvent> for PyElement {
    fn from(event: KafkaEvent) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            // Every insert is a fresh `str` key into a fresh dict, so the only
            // failure mode `set_item` documents is unreachable here.
            let set = |key: &str, value: Bound<'_, PyAny>| {
                dict.set_item(key, value)
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            };
            // Primitive `IntoPyObject` conversions are infallible, so the
            // `expect`s below are unreachable invariants (as in `PyElement`'s
            // own scalar edges).
            const INFALLIBLE: &str = "invariant: scalar -> PyObject conversion is infallible";
            set(
                "topic",
                event.topic.into_pyobject(py).expect(INFALLIBLE).into_any(),
            );
            set(
                "partition",
                event
                    .partition
                    .into_pyobject(py)
                    .expect(INFALLIBLE)
                    .into_any(),
            );
            set(
                "offset",
                event.offset.into_pyobject(py).expect(INFALLIBLE).into_any(),
            );
            set(
                "key",
                match &event.key {
                    Some(k) => PyBytes::new(py, k).into_any(),
                    None => py.None().into_bound(py),
                },
            );
            set("value", PyBytes::new(py, &event.value).into_any());
            PyElement::new(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Writing: a Python dict -> a KafkaRecord.
// ---------------------------------------------------------------------------

/// Marshal one Python `dict` into a [`KafkaRecord`].
///
/// `topic` falls back to `default_topic` when the dict omits it; `key` is
/// optional (absent or `None` → no key); `value` is required. Every failure
/// names the field, so a malformed value aborts the run rather than publishing
/// an empty or mis-topiced record.
fn dict_to_record(dict: &Bound<'_, PyDict>, default_topic: Option<&str>) -> Result<KafkaRecord> {
    let record = RecordDict::new(dict, WHO);
    Ok(KafkaRecord {
        topic: record.str_or("topic", default_topic, "topic")?,
        key: record.opt_bytes("key")?,
        value: record.bytes("value")?,
    })
}

/// Marshal one erased stream value — a Python `dict` — into a record.
fn element_to_record(elem: &PyElement, default_topic: Option<&str>) -> Result<KafkaRecord> {
    Python::attach(|py| {
        let dict = record_dict(
            elem,
            py,
            WHO,
            "with 'value' (and optionally 'topic' / 'key')",
        )?;
        dict_to_record(&dict, default_topic)
    })
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Consume a Kafka topic as a real-time source.
///
/// Each tick yields a `list` of event dicts —
/// `{"topic": str, "partition": int, "offset": int, "key": bytes | None,
/// "value": bytes}` — holding the messages that arrived between graph cycles.
/// Call `[0]` for the single-message case.
///
/// The same `group_id` across runs continues from the last committed offset; a
/// fresh group reads from the beginning. Delivery is at-most-once.
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. A Kafka consumer is a live, unbounded,
/// wall-clock-stamped stream with no historical timeline, so `realtime=False`
/// raises here.
#[pyadapter(name = kafka_sub, source)]
#[pyo3(signature = (brokers, topic, group_id, realtime = true))]
fn sub(
    g: &GraphBuilder,
    brokers: String,
    topic: String,
    group_id: String,
    realtime: bool,
) -> Result<Stream<Burst<KafkaEvent>>> {
    kafka_subscribe(
        g,
        run_mode(realtime),
        KafkaConnection::new(brokers),
        topic,
        group_id,
    )
}

/// Produce this stream to Kafka.
///
/// Each tick's value is either a single `dict` or a `list`/`tuple` of dicts for
/// several messages at one timestamp. A record dict is
/// `{"value": bytes, "topic": str (optional), "key": bytes (optional)}`.
///
/// Each record names its own target topic, so one sink can write to many;
/// `topic` supplies the default for dicts that omit it. A dict with neither a
/// `"topic"` key nor a `topic` default is an error, as is a missing `"value"`, a
/// wrong-typed field, or a non-dict stream value — the run aborts rather than
/// publishing a malformed record.
///
/// librdkafka connects lazily, so an unreachable broker surfaces as a produce
/// failure during the run, not at wiring; a client-config error still raises
/// here. Returns a terminal stream whose value is `None`.
#[pyadapter(name = kafka_pub)]
#[pyo3(signature = (brokers, topic = None))]
fn publish(
    stream: &Stream<Burst<PyElement>>,
    brokers: String,
    topic: Option<String>,
) -> Result<Stream<()>> {
    let records: Stream<Burst<KafkaRecord>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(|elem| element_to_record(elem, topic.as_deref()))
            .collect::<Result<Burst<KafkaRecord>>>()
    });
    records.kafka_pub(KafkaConnection::new(brokers))
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

    fn bytes_obj(py: Python<'_>, bytes: &[u8]) -> Py<PyAny> {
        PyBytes::new(py, bytes).into_any().unbind()
    }

    fn str_obj(py: Python<'_>, s: &str) -> Py<PyAny> {
        s.into_pyobject(py).unwrap().into_any().unbind()
    }

    #[test]
    fn event_erases_to_a_dict() {
        let event = KafkaEvent {
            topic: "trades".into(),
            partition: 3,
            offset: 42,
            key: Some(b"AAPL".to_vec()),
            value: b"payload".to_vec(),
        };
        let element = PyElement::from(event);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("trades", get("topic").extract::<String>().unwrap());
            assert_eq!(3, get("partition").extract::<i32>().unwrap());
            assert_eq!(42, get("offset").extract::<i64>().unwrap());
            assert_eq!(b"AAPL".to_vec(), get("key").extract::<Vec<u8>>().unwrap());
            assert_eq!(
                b"payload".to_vec(),
                get("value").extract::<Vec<u8>>().unwrap()
            );
        });
    }

    #[test]
    fn an_absent_key_erases_to_none() {
        let event = KafkaEvent {
            topic: "t".into(),
            value: b"v".to_vec(),
            ..Default::default()
        };
        let element = PyElement::from(event);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            assert!(dict.get_item("key").unwrap().unwrap().is_none());
        });
    }

    #[test]
    fn a_value_erases_to_bytes_not_a_list_of_ints() {
        let event = KafkaEvent {
            value: vec![1, 2, 255],
            ..Default::default()
        };
        let element = PyElement::from(event);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let value = dict.get_item("value").unwrap().unwrap();
            assert!(value.is_instance_of::<PyBytes>(), "got {value:?}");
        });
    }

    #[test]
    fn dict_marshals_to_a_record() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("topic", str_obj(py, "trades")),
                    ("key", bytes_obj(py, b"AAPL")),
                    ("value", bytes_obj(py, b"payload")),
                ],
            );
            let record = element_to_record(&element, None).unwrap();
            assert_eq!("trades", record.topic);
            assert_eq!(Some(b"AAPL".to_vec()), record.key);
            assert_eq!(b"payload".to_vec(), record.value);
        });
    }

    #[test]
    fn the_default_topic_fills_in_for_a_dict_without_one() {
        Python::attach(|py| {
            let element = dict_element(py, &[("value", bytes_obj(py, b"v"))]);
            let record = element_to_record(&element, Some("fallback")).unwrap();
            assert_eq!("fallback", record.topic);
            assert_eq!(None, record.key);
        });
    }

    #[test]
    fn a_dict_topic_wins_over_the_default() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("topic", str_obj(py, "own")),
                    ("value", bytes_obj(py, b"v")),
                ],
            );
            let record = element_to_record(&element, Some("fallback")).unwrap();
            assert_eq!("own", record.topic);
        });
    }

    #[test]
    fn no_topic_and_no_default_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("value", bytes_obj(py, b"v"))]);
            let err = element_to_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("no 'topic'"), "got: {err}");
        });
    }

    #[test]
    fn a_missing_value_errors_rather_than_publishing_empty_bytes() {
        Python::attach(|py| {
            let element = dict_element(py, &[("topic", str_obj(py, "t"))]);
            let err = element_to_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("missing 'value'"), "got: {err}");
        });
    }

    #[test]
    fn a_wrong_typed_value_errors_rather_than_being_dropped() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("topic", str_obj(py, "t")),
                    ("value", str_obj(py, "not-bytes")),
                ],
            );
            let err = element_to_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("'value'"), "got: {err}");
        });
    }

    #[test]
    fn a_wrong_typed_key_errors_rather_than_being_dropped() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("topic", str_obj(py, "t")),
                    ("key", 7_i64.into_pyobject(py).unwrap().into_any().unbind()),
                    ("value", bytes_obj(py, b"v")),
                ],
            );
            let err = element_to_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("'key'"), "got: {err}");
        });
    }

    #[test]
    fn an_explicit_none_key_is_no_key() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("topic", str_obj(py, "t")),
                    ("key", py.None()),
                    ("value", bytes_obj(py, b"v")),
                ],
            );
            assert_eq!(None, element_to_record(&element, None).unwrap().key);
        });
    }

    #[test]
    fn a_non_dict_value_errors_rather_than_publishing_nothing() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_record(&element, None).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "got: {err}");
        });
    }

    #[test]
    fn an_empty_element_errors() {
        let err = element_to_record(&PyElement::none(), None).unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }
}
