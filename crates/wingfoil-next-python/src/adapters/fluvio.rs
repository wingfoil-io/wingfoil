//! Python bindings for the wingfoil-next **Fluvio** adapter
//! ([`wingfoil_next::adapters::fluvio`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                    | Rust               | shape |
//! |---------------------------|--------------------|-------|
//! | `fluvio_sub(graph, …)`    | [`fluvio_sub`]     | real-time partition consumer |
//! | `fluvio_pub(stream, …)`   | [`FluvioSinkOps`]  | topic producer sink |
//!
//! # The dynamic edge
//!
//! [`FluvioEvent`] and [`FluvioRecord`] are concrete on the Rust side, so only
//! the *shape* is dynamic:
//!
//! - **reads** erase [`FluvioEvent`] to
//!   `{"key": bytes | None, "value": bytes, "offset": int}` via a plain
//!   `From<FluvioEvent> for PyElement`, so the source goes through the standard
//!   `erase_burst_source` seam with no intermediate type;
//! - **writes** marshal a Python `dict` into a [`FluvioRecord`] via
//!   [`RecordDict`]. The sink takes no per-record fallback argument (a Fluvio
//!   topic is named once, on the sink, not per record), but the marshaling
//!   still has to happen inside the fn, since `Burst<PyElement>` is what the
//!   `typed_burst_input` seam can hand it.
//!
//! **The key is asymmetric**, and this is the adapter's shape rather than a
//! binding choice: a consumed `FluvioEvent.key` is `Option<Vec<u8>>` (raw
//! bytes off the wire) while a produced `FluvioRecord.key` is `Option<String>`
//! (Fluvio's `RecordKey` is textual). So a read yields `bytes | None` and a
//! write expects `str | None` — round-tripping a key needs an explicit
//! `.decode()`.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Marshaling fails loudly.** Legacy's `dict_to_record` turned a missing or
//!    wrong-typed `value` into empty bytes, a wrong-typed `key` into no key at
//!    all, and a non-dict stream value into an *empty burst* that produced
//!    nothing. Each of those now aborts the run naming the field.
//! 2. **The run mode is an argument.** `fluvio_sub` rejects a historical run at
//!    wiring (a live consumer has no timeline to replay), and a Python `Graph`
//!    does not know its mode until `run()` — so `realtime` is explicit and must
//!    match the eventual `graph.run(realtime=…)`. `realtime=False` raises here.
//! 3. **New capability: `buffer_size`** on the sink — the engine adapter's
//!    back-pressure bound (`None` = unbounded), which legacy had no equivalent
//!    for.

use anyhow::Result;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use wingfoil_next::adapters::fluvio::{
    FluvioConnection, FluvioEvent, FluvioRecord, FluvioSinkOps, fluvio_sub as fluvio_consume,
};
use wingfoil_next::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::{RecordDict, record_dict, run_mode};
use crate::{PyElement, pyadapter};

/// The error-message prefix for the sink's marshaling failures.
const WHO: &str = "fluvio_pub";

// ---------------------------------------------------------------------------
// Reading: an event -> a Python dict.
// ---------------------------------------------------------------------------

/// A consumed record erases to `{"key": bytes | None, "value": bytes,
/// "offset": int}`. Both payloads stay `bytes` — Fluvio records are opaque, so
/// this does not guess at UTF-8.
impl From<FluvioEvent> for PyElement {
    fn from(event: FluvioEvent) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            let set = |key: &str, value: Bound<'_, PyAny>| {
                dict.set_item(key, value)
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            };
            set(
                "key",
                match &event.key {
                    Some(k) => PyBytes::new(py, k).into_any(),
                    None => py.None().into_bound(py),
                },
            );
            set("value", PyBytes::new(py, &event.value).into_any());
            set(
                "offset",
                event
                    .offset
                    .into_pyobject(py)
                    .expect("invariant: scalar -> PyObject conversion is infallible")
                    .into_any(),
            );
            PyElement::new(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Writing: a Python dict -> a FluvioRecord.
// ---------------------------------------------------------------------------

/// Marshal one erased stream value into a record.
///
/// `key` is optional and **textual** (see the module docs on the asymmetry);
/// `value` is required bytes.
fn element_to_record(elem: &PyElement) -> Result<FluvioRecord> {
    Python::attach(|py| {
        let dict = record_dict(elem, py, WHO, "with 'value' (and optionally 'key')")?;
        let record = RecordDict::new(&dict, WHO);
        Ok(FluvioRecord {
            key: record.opt_str("key")?,
            value: record.bytes("value")?,
        })
    })
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Consume a Fluvio topic partition as a real-time source.
///
/// Each tick yields a `list` of
/// `{"key": bytes | None, "value": bytes, "offset": int}` dicts — the records
/// that arrived between graph cycles. Call `[0]` for the single-record case.
///
/// `start_offset` is the absolute partition offset to start from; `None` starts
/// from the beginning. A negative offset is rejected at wiring.
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. A Fluvio consumer is a live, unbounded,
/// wall-clock-stamped stream with no historical timeline, so `realtime=False`
/// raises here.
///
/// The cluster connection is made at run start, so an unreachable SC or a
/// missing topic aborts the run rather than raising at wiring.
#[pyadapter(name = fluvio_sub, source)]
#[pyo3(signature = (endpoint, topic, partition = 0, start_offset = None, realtime = true))]
fn sub(
    g: &GraphBuilder,
    endpoint: String,
    topic: String,
    partition: u32,
    start_offset: Option<i64>,
    realtime: bool,
) -> Result<Stream<Burst<FluvioEvent>>> {
    fluvio_consume(
        g,
        run_mode(realtime),
        FluvioConnection::new(endpoint),
        topic,
        partition,
        start_offset,
    )
}

/// Produce this stream to a Fluvio topic.
///
/// Each tick's value is either a single `dict` or a `list`/`tuple` of dicts for
/// several records at one timestamp. A record dict is
/// `{"value": bytes, "key": str (optional)}` — note the key is a **`str`** here
/// while a consumed key reads back as `bytes` (see the module docs).
///
/// A missing `"value"`, a wrong-typed field, or a non-dict stream value aborts
/// the run rather than producing an empty or keyless record by accident.
///
/// The topic must already exist. `buffer_size` bounds the unwritten backlog
/// (`None` = unbounded). The cluster connection and producer are established
/// lazily on the first burst, so an unreachable SC or a missing topic aborts the
/// run rather than raising at wiring. Returns a terminal stream whose value is
/// `None`.
#[pyadapter(name = fluvio_pub)]
#[pyo3(signature = (endpoint, topic, buffer_size = None))]
fn publish(
    stream: &Stream<Burst<PyElement>>,
    endpoint: String,
    topic: String,
    buffer_size: Option<usize>,
) -> Result<Stream<()>> {
    let records: Stream<Burst<FluvioRecord>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(element_to_record)
            .collect::<Result<Burst<FluvioRecord>>>()
    });
    records.fluvio_pub(FluvioConnection::new(endpoint), &topic, buffer_size)
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

    #[test]
    fn an_event_erases_to_a_dict() {
        let element = PyElement::from(FluvioEvent {
            key: Some(b"k".to_vec()),
            value: b"payload".to_vec(),
            offset: 12,
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!(b"k".to_vec(), get("key").extract::<Vec<u8>>().unwrap());
            assert_eq!(
                b"payload".to_vec(),
                get("value").extract::<Vec<u8>>().unwrap()
            );
            assert_eq!(12, get("offset").extract::<i64>().unwrap());
            assert!(get("value").is_instance_of::<PyBytes>());
        });
    }

    #[test]
    fn a_keyless_event_erases_with_a_none_key() {
        let element = PyElement::from(FluvioEvent {
            value: b"v".to_vec(),
            ..Default::default()
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            assert!(dict.get_item("key").unwrap().unwrap().is_none());
        });
    }

    #[test]
    fn a_dict_marshals_to_a_record_with_a_textual_key() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[("key", str_obj(py, "k")), ("value", bytes_obj(py, b"v"))],
            );
            let record = element_to_record(&element).unwrap();
            assert_eq!(Some("k".to_string()), record.key);
            assert_eq!(b"v".to_vec(), record.value);
        });
    }

    #[test]
    fn an_absent_or_none_key_is_keyless() {
        Python::attach(|py| {
            let absent = dict_element(py, &[("value", bytes_obj(py, b"v"))]);
            assert_eq!(None, element_to_record(&absent).unwrap().key);

            let explicit = dict_element(py, &[("key", py.None()), ("value", bytes_obj(py, b"v"))]);
            assert_eq!(None, element_to_record(&explicit).unwrap().key);
        });
    }

    #[test]
    fn a_missing_value_errors_rather_than_producing_empty_bytes() {
        Python::attach(|py| {
            let element = dict_element(py, &[("key", str_obj(py, "k"))]);
            let err = element_to_record(&element).unwrap_err();
            assert!(err.to_string().contains("missing 'value'"), "got: {err}");
        });
    }

    #[test]
    fn a_bytes_key_errors_rather_than_being_dropped() {
        // The asymmetry made explicit: a produced key is textual, so handing
        // back a consumed key verbatim is an error, not a silent keyless send.
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[("key", bytes_obj(py, b"k")), ("value", bytes_obj(py, b"v"))],
            );
            let err = element_to_record(&element).unwrap_err();
            assert!(
                err.to_string().contains("'key' must be a str"),
                "got: {err}"
            );
        });
    }

    #[test]
    fn a_wrong_typed_value_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("value", str_obj(py, "not-bytes"))]);
            assert!(element_to_record(&element).is_err());
        });
    }

    #[test]
    fn a_non_dict_value_errors_rather_than_producing_nothing() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_record(&element).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "got: {err}");
        });
    }

    #[test]
    fn an_empty_element_errors() {
        let err = element_to_record(&PyElement::none()).unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }
}
