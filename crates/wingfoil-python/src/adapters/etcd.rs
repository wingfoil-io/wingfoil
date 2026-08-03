//! Python bindings for the wingfoil **etcd** adapter
//! ([`wingfoil::adapters::etcd`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                  | Rust             | shape |
//! |-------------------------|------------------|-------|
//! | `etcd_sub(graph, …)`    | [`etcd_sub`]     | prefix snapshot then live watch |
//! | `etcd_pub(stream, …)`   | [`EtcdSinkOps`]  | `PUT` sink, optionally leased |
//!
//! # The dynamic edge
//!
//! [`EtcdEvent`] is concrete on the Rust side, so only the *shape* is dynamic:
//!
//! - **reads** erase to `{"kind": "put" | "delete", "key": str,
//!   "value": bytes, "revision": int}` via a plain `From<EtcdEvent> for
//!   PyElement`, so the source goes through the standard `erase_burst_source`
//!   seam with no intermediate type;
//! - **writes** marshal a Python `dict` into an [`EtcdEntry`] via
//!   [`RecordDict`]. Both fields are required — an etcd key has no meaningful
//!   binding-level fallback — so this uses `RecordDict::str` rather than the
//!   `str_or` fallback shape the kafka/redis sinks take.
//!
//! `kind` is a **string** rather than a `#[pyclass]` enum, the convention
//! postgres set for its SQL column types: the module surface stays small and
//! there is no extra class to register.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Marshaling fails loudly.** Legacy's `dict_to_entry` defaulted through
//!    every error — a missing or wrong-typed `key` became `""` (a write to the
//!    empty key), a missing `value` became empty bytes, and a non-dict stream
//!    value logged and produced an *empty burst*, writing nothing. Each of those
//!    now aborts the run naming the field.
//! 2. **The run mode is an argument.** `etcd_sub` rejects a historical run at
//!    wiring (the watch is live and unbounded), and a Python `Graph` does not
//!    know its mode until `run()` — so `realtime` is explicit and must match the
//!    eventual `graph.run(realtime=…)`. `realtime=False` raises here.
//! 3. **Multiple endpoints.** `endpoints` accepts a single `str` *or* a list of
//!    them, exposing the engine's [`EtcdConnection::with_endpoints`] cluster
//!    support; legacy took one endpoint only.

use std::time::Duration;

use anyhow::{Result, anyhow};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};
use wingfoil::adapters::etcd::{
    EtcdConnection, EtcdEntry, EtcdEvent, EtcdEventKind, EtcdSinkOps, etcd_sub as etcd_watch,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::{RecordDict, record_dict, run_mode};
use crate::{PyElement, pyadapter};

/// The error-message prefix for the sink's marshaling failures.
const WHO: &str = "etcd_pub";

// ---------------------------------------------------------------------------
// Connection: a str or a list of them.
// ---------------------------------------------------------------------------

/// Build the connection from a Python `str` **or** a sequence of them.
///
/// A single endpoint is by far the common case (and what legacy accepted), but
/// an etcd cluster has several and the engine's `EtcdConnection` already models
/// that — so both spellings are accepted rather than forcing every caller to
/// wrap a lone endpoint in a list.
fn connection(endpoints: &Bound<'_, PyAny>) -> Result<EtcdConnection> {
    if let Ok(single) = endpoints.cast::<PyString>() {
        let endpoint = single
            .extract::<String>()
            .map_err(|e| anyhow!("etcd: endpoint must be a str: {e}"))?;
        return Ok(EtcdConnection::new(endpoint));
    }
    if endpoints.is_instance_of::<PyList>() || endpoints.is_instance_of::<PyTuple>() {
        let list = endpoints
            .extract::<Vec<String>>()
            .map_err(|e| anyhow!("etcd: endpoints must be a list of str: {e}"))?;
        if list.is_empty() {
            return Err(anyhow!("etcd: endpoints list is empty"));
        }
        return Ok(EtcdConnection::with_endpoints(list));
    }
    Err(anyhow!(
        "etcd: endpoints must be a str or a list of str, got {}",
        endpoints.get_type()
    ))
}

// ---------------------------------------------------------------------------
// Reading: an event -> a Python dict.
// ---------------------------------------------------------------------------

/// A watch event erases to
/// `{"kind": "put" | "delete", "key": str, "value": bytes, "revision": int}`.
///
/// A `delete` event carries its key but an **empty** value — etcd does not
/// report the prior value — so an empty `value` on a delete is expected, not a
/// marshaling failure.
impl From<EtcdEvent> for PyElement {
    fn from(event: EtcdEvent) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            let set = |key: &str, value: Bound<'_, PyAny>| {
                dict.set_item(key, value)
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            };
            const INFALLIBLE: &str = "invariant: scalar -> PyObject conversion is infallible";
            let kind = match event.kind {
                EtcdEventKind::Put => "put",
                EtcdEventKind::Delete => "delete",
            };
            set("kind", kind.into_pyobject(py).expect(INFALLIBLE).into_any());
            set(
                "key",
                event
                    .entry
                    .key
                    .into_pyobject(py)
                    .expect(INFALLIBLE)
                    .into_any(),
            );
            set("value", PyBytes::new(py, &event.entry.value).into_any());
            set(
                "revision",
                event
                    .revision
                    .into_pyobject(py)
                    .expect(INFALLIBLE)
                    .into_any(),
            );
            PyElement::new(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Writing: a Python dict -> an EtcdEntry.
// ---------------------------------------------------------------------------

/// Marshal one erased stream value into an entry. Both fields are required: an
/// etcd key has no sensible fallback, and legacy's `""` default wrote to the
/// empty key rather than failing.
fn element_to_entry(elem: &PyElement) -> Result<EtcdEntry> {
    Python::attach(|py| {
        let dict = record_dict(elem, py, WHO, "with 'key' and 'value'")?;
        let record = RecordDict::new(&dict, WHO);
        Ok(EtcdEntry {
            key: record.str("key")?,
            value: record.bytes("value")?,
        })
    })
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Watch an etcd key prefix: a consistent snapshot of everything under it, then
/// the live updates.
///
/// Each tick yields a `list` of
/// `{"kind": "put" | "delete", "key": str, "value": bytes, "revision": int}`
/// dicts. Snapshot entries are always `"put"`. A `"delete"` carries its key with
/// an **empty** `value` — etcd does not report the prior value.
///
/// The watch is opened before the snapshot GET, so no write is missed in the
/// handoff; watch events already covered by the snapshot are filtered out.
///
/// `endpoints` is a single endpoint (`"http://localhost:2379"`) or a list of
/// them for a cluster. `realtime` declares the run mode this source is wired
/// for and must match the eventual `graph.run(realtime=…)`; the watch is live
/// and unbounded, so `realtime=False` raises here.
#[pyadapter(name = etcd_sub, source)]
#[pyo3(signature = (endpoints, prefix, realtime = true))]
fn sub(
    g: &GraphBuilder,
    endpoints: Py<PyAny>,
    prefix: String,
    realtime: bool,
) -> Result<Stream<Burst<EtcdEvent>>> {
    let conn = Python::attach(|py| connection(endpoints.bind(py)))?;
    etcd_watch(g, run_mode(realtime), conn, prefix)
}

/// Write this stream to etcd with `PUT`.
///
/// Each tick's value is either a single `dict` or a `list`/`tuple` of dicts for
/// several writes at one timestamp. An entry dict is
/// `{"key": str, "value": bytes}` — **both required**. A missing or wrong-typed
/// field, or a non-dict stream value, aborts the run rather than writing to the
/// empty key or with an empty value.
///
/// `lease_ttl_secs` attaches a lease with automatic keepalive renewal, so the
/// keys vanish when the sink tears down (`None` = plain writes that outlive the
/// graph). `force=False` makes each write conditional: an existing key aborts
/// the run naming it, rather than silently overwriting.
///
/// The connection and any lease are established lazily on the first write, so a
/// connection or `lease_grant` failure aborts the run rather than raising at
/// wiring. Returns a terminal stream whose value is `None`.
#[pyadapter(name = etcd_pub)]
#[pyo3(signature = (endpoints, lease_ttl_secs = None, force = true))]
fn publish(
    stream: &Stream<Burst<PyElement>>,
    endpoints: Py<PyAny>,
    lease_ttl_secs: Option<f64>,
    force: bool,
) -> Result<Stream<()>> {
    let conn = Python::attach(|py| connection(endpoints.bind(py)))?;
    let lease = match lease_ttl_secs {
        Some(secs) if !secs.is_finite() || secs <= 0.0 => {
            return Err(anyhow!(
                "etcd_pub: lease_ttl_secs must be a finite, positive number, got {secs}"
            ));
        }
        Some(secs) => Some(Duration::from_secs_f64(secs)),
        None => None,
    };
    let entries: Stream<Burst<EtcdEntry>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(element_to_entry)
            .collect::<Result<Burst<EtcdEntry>>>()
    });
    entries.etcd_pub(conn, lease, force)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_put_event_erases_to_a_dict() {
        let element = PyElement::from(EtcdEvent {
            kind: EtcdEventKind::Put,
            entry: EtcdEntry {
                key: "/app/a".into(),
                value: b"v".to_vec(),
            },
            revision: 7,
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("put", get("kind").extract::<String>().unwrap());
            assert_eq!("/app/a", get("key").extract::<String>().unwrap());
            assert_eq!(b"v".to_vec(), get("value").extract::<Vec<u8>>().unwrap());
            assert_eq!(7, get("revision").extract::<i64>().unwrap());
            assert!(get("value").is_instance_of::<PyBytes>());
        });
    }

    #[test]
    fn a_delete_event_erases_with_an_empty_value() {
        let element = PyElement::from(EtcdEvent {
            kind: EtcdEventKind::Delete,
            entry: EtcdEntry {
                key: "/app/a".into(),
                value: vec![],
            },
            revision: 9,
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("delete", get("kind").extract::<String>().unwrap());
            assert_eq!(Vec::<u8>::new(), get("value").extract::<Vec<u8>>().unwrap());
        });
    }

    #[test]
    fn a_dict_marshals_to_an_entry() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("key", str_obj(py, "/app/a")),
                    ("value", bytes_obj(py, b"v")),
                ],
            );
            let entry = element_to_entry(&element).unwrap();
            assert_eq!("/app/a", entry.key);
            assert_eq!(b"v".to_vec(), entry.value);
        });
    }

    #[test]
    fn a_missing_key_errors_rather_than_writing_the_empty_key() {
        Python::attach(|py| {
            let element = dict_element(py, &[("value", bytes_obj(py, b"v"))]);
            let err = element_to_entry(&element).unwrap_err();
            assert!(err.to_string().contains("missing 'key'"), "got: {err}");
        });
    }

    #[test]
    fn a_missing_value_errors_rather_than_writing_empty_bytes() {
        Python::attach(|py| {
            let element = dict_element(py, &[("key", str_obj(py, "/app/a"))]);
            let err = element_to_entry(&element).unwrap_err();
            assert!(err.to_string().contains("missing 'value'"), "got: {err}");
        });
    }

    #[test]
    fn a_wrong_typed_key_errors() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("key", 7_i64.into_pyobject(py).unwrap().into_any().unbind()),
                    ("value", bytes_obj(py, b"v")),
                ],
            );
            assert!(element_to_entry(&element).is_err());
        });
    }

    #[test]
    fn a_non_dict_value_errors_rather_than_writing_nothing() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_entry(&element).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "got: {err}");
        });
    }

    #[test]
    fn a_str_endpoint_and_a_list_both_build_a_connection() {
        Python::attach(|py| {
            let single = "http://localhost:2379"
                .into_pyobject(py)
                .unwrap()
                .into_any();
            assert_eq!(
                vec!["http://localhost:2379".to_string()],
                connection(&single).unwrap().endpoints
            );

            let many = PyList::new(py, ["http://a:2379", "http://b:2379"])
                .unwrap()
                .into_any();
            assert_eq!(
                vec!["http://a:2379".to_string(), "http://b:2379".to_string()],
                connection(&many).unwrap().endpoints
            );
        });
    }

    #[test]
    fn a_bad_endpoints_argument_errors() {
        Python::attach(|py| {
            let number = 7_i64.into_pyobject(py).unwrap().into_any();
            assert!(connection(&number).is_err());

            let empty = PyList::empty(py).into_any();
            let err = connection(&empty).unwrap_err();
            assert!(err.to_string().contains("empty"), "got: {err}");
        });
    }
}
