//! Python bindings for the wingfoil-next **FIX** adapter
//! ([`wingfoil_next::adapters::fix`]).
//!
//! Four graph entry points plus one handle class:
//!
//! | Python                     | Rust                 | shape |
//! |----------------------------|----------------------|-------|
//! | `fix_connect(graph, …)`    | [`fix_connect`]      | initiator → `(data, status)` |
//! | `fix_accept(graph, …)`     | [`fix_accept`]       | acceptor → `(data, status)` |
//! | `fix_send(stream, …)`      | [`FixOperators`]     | dedicated outbound session sink |
//! | `fix_connect_tls(graph, …)`| [`fix_connect_tls`]  | TLS initiator → a `FixConnection` |
//!
//! The first three are `#[pyadapter]`-generated. The fourth is **not** a shape
//! the macro can generate: the Rust adapter hands back a [`FixConnection`]
//! object carrying two streams *and* a live sender, so it is hand-written as a
//! `#[pyfunction]` returning a `#[pyclass]` — the same handle shape web's
//! `WebServer` and prometheus's `PrometheusExporter` will need.
//!
//! # The message edge
//!
//! A FIX message erases to a `dict`:
//!
//! ```python
//! {"msg_type": "W", "seq_num": 12, "fields": [(55, "EURUSD"), (270, "1.0842")]}
//! ```
//!
//! and the same shape marshals back for `fix_send` / `FixConnection.send` —
//! `seq_num` is ignored on the way out, since the session stamps it. Tag values
//! are `str` on both edges: FIX is a text protocol and every value goes on the
//! wire as characters, so a number must be spelled `str(price)` rather than
//! silently coerced. Field pairs may be tuples *or* lists (legacy documented
//! lists), or any 2-element iterable.
//!
//! Both source streams are burst-shaped, so each tick yields a **`list`** — the
//! messages, or status transitions, seen between graph cycles.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Session status is always a `dict`.** Legacy erased three of the five
//!    states to a bare string and the other two to a dict, so a consumer had to
//!    type-check before reading. Here every status is
//!    `{"status": "...", ...}` — `logged_out` adds `"reason"`, `error` adds
//!    `"message"`, and the rest carry `status` alone.
//! 2. **The run mode is an argument.** Every FIX session is live, so a
//!    historical run is rejected at *wiring*; a Python `Graph` does not know its
//!    mode until `run()`, so `realtime` is explicit. `realtime=False` raises.
//! 3. **Marshaling fails loudly.** A missing `msg_type`, a malformed field pair,
//!    or a non-`str` tag value aborts naming the cause. Legacy raised on some of
//!    these and silently accepted others.
//! 4. **New capabilities**: `fix_send` (the dedicated outbound-session sink),
//!    `FixConnection.fix_sub` (market-data subscription driven by a symbol
//!    stream, which legacy never bound), and the `poll_mode` selector — legacy
//!    hardcoded `Threaded`.

use anyhow::{Context, Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use wingfoil_next::adapters::fix::{
    FixConnection, FixMessage, FixOperators, FixPollMode, FixSessionStatus, fix_accept as fix_bind,
    fix_connect as fix_dial, fix_connect_tls as fix_dial_tls,
};
use wingfoil_next::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::run_mode;
use crate::{Graph, PyElement, PyStream, pyadapter};

// ---------------------------------------------------------------------------
// Reading: a FIX message / status -> a Python dict.
// ---------------------------------------------------------------------------

/// Insert into a freshly-made dict, where the only failure mode is an
/// unhashable key — and every key here is a `&str` literal.
fn put(dict: &Bound<'_, PyDict>, key: &str, value: impl for<'py> IntoPyObject<'py>) {
    dict.set_item(key, value)
        .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
}

impl From<FixMessage> for PyElement {
    fn from(msg: FixMessage) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            put(&dict, "msg_type", msg.msg_type.as_str());
            put(&dict, "seq_num", msg.seq_num);
            let fields: Vec<(u32, &str)> =
                msg.fields.iter().map(|(t, v)| (*t, v.as_str())).collect();
            put(&dict, "fields", fields);
            PyElement::new(dict.into_any().unbind())
        })
    }
}

impl From<FixSessionStatus> for PyElement {
    fn from(status: FixSessionStatus) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            // Uniformly a dict: `status` always present, with the payload keys
            // only the two carrying states add (see the module docs).
            match &status {
                FixSessionStatus::Disconnected => put(&dict, "status", "disconnected"),
                FixSessionStatus::LoggingIn => put(&dict, "status", "logging_in"),
                FixSessionStatus::LoggedIn => put(&dict, "status", "logged_in"),
                FixSessionStatus::LoggedOut(reason) => {
                    put(&dict, "status", "logged_out");
                    put(&dict, "reason", reason.as_deref());
                }
                FixSessionStatus::Error(message) => {
                    put(&dict, "status", "error");
                    put(&dict, "message", message.as_str());
                }
            }
            PyElement::new(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Writing: a Python dict -> a FIX message.
// ---------------------------------------------------------------------------

/// Marshal one erased stream value — a Python `dict` — into a [`FixMessage`].
///
/// `seq_num` is not read: the session owns sequence numbering and stamps it at
/// send time, so a caller-supplied one would be silently overwritten.
fn element_to_message(elem: &PyElement, who: &'static str) -> Result<FixMessage> {
    if elem.is_none() {
        bail!("{who}: stream value is empty (the upstream produced no value)");
    }
    Python::attach(|py| {
        let obj = elem.object().bind(py);
        let dict = obj.cast::<PyDict>().map_err(|_| {
            anyhow!(
                "{who}: value must be a dict with 'msg_type' and 'fields', got {}",
                obj.get_type()
            )
        })?;
        message_from_dict(dict, who)
    })
}

/// The dict → [`FixMessage`] read itself, shared by the sink and
/// [`PyFixConnection::send`].
fn message_from_dict(dict: &Bound<'_, PyDict>, who: &'static str) -> Result<FixMessage> {
    let msg_type = dict
        .get_item("msg_type")
        .map_err(|e| anyhow!("{who}: reading 'msg_type': {e}"))?
        .ok_or_else(|| anyhow!("{who}: message is missing 'msg_type'"))?
        .extract::<String>()
        .map_err(|e| anyhow!("{who}: 'msg_type' must be a str: {e}"))?;

    let fields_obj = dict
        .get_item("fields")
        .map_err(|e| anyhow!("{who}: reading 'fields': {e}"))?
        .ok_or_else(|| anyhow!("{who}: message is missing 'fields'"))?;

    // Deliberately iterated by hand rather than `extract::<Vec<(u32, String)>>()`:
    // pyo3's tuple conversion accepts only a real `tuple`, and legacy documented
    // (and its tests used) `[tag, value]` lists.
    let mut fields = Vec::new();
    let pairs = fields_obj
        .try_iter()
        .map_err(|e| anyhow!("{who}: 'fields' must be a list of (tag, value) pairs: {e}"))?;
    for (index, pair) in pairs.enumerate() {
        let pair = pair.map_err(|e| anyhow!("{who}: 'fields'[{index}]: {e}"))?;
        let parts = pair
            .try_iter()
            .map_err(|e| anyhow!("{who}: 'fields'[{index}] must be a (tag, value) pair: {e}"))?
            .collect::<PyResult<Vec<_>>>()
            .map_err(|e| anyhow!("{who}: 'fields'[{index}]: {e}"))?;
        if parts.len() != 2 {
            bail!(
                "{who}: 'fields'[{index}] must be a (tag, value) pair, got {} items",
                parts.len()
            );
        }
        let tag = parts[0]
            .extract::<u32>()
            .map_err(|e| anyhow!("{who}: 'fields'[{index}] tag must be an int: {e}"))?;
        let value = parts[1].extract::<String>().map_err(|e| {
            anyhow!("{who}: 'fields'[{index}] value for tag {tag} must be a str: {e}")
        })?;
        fields.push((tag, value));
    }

    Ok(FixMessage {
        msg_type,
        seq_num: 0,
        sending_time: wingfoil::NanoTime::ZERO,
        fields,
    })
}

/// The poll-mode selector, a string rather than a `#[pyclass]` enum.
fn poll_mode(name: &str) -> Result<FixPollMode> {
    match name {
        "threaded" => Ok(FixPollMode::Threaded),
        "spin" => Ok(FixPollMode::AlwaysSpin),
        other => bail!("fix: unknown poll_mode '{other}'; expected 'threaded' or 'spin'"),
    }
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Connect to a FIX acceptor as an initiator over plain TCP.
///
/// Returns a **`(data, status)` tuple**:
///
/// - `data` ticks with a `list` of message dicts — the application messages
///   received between graph cycles, losslessly grouped;
/// - `status` ticks with a `list` of session-status dicts (see the module docs).
///
/// ```python
/// data, status = fix_connect(g, "localhost", 9876, "CLIENT", "SERVER")
/// ```
///
/// `poll_mode` is `"threaded"` (a background reader thread, the legacy default)
/// or `"spin"` (the graph spin loop drives polling — lowest latency, one core).
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. A FIX session is live with no historical
/// timeline, so `realtime=False` raises here.
#[pyadapter(name = fix_connect, source)]
#[pyo3(signature = (
    host, port, sender_comp_id, target_comp_id,
    realtime = true, poll_mode = "threaded".to_string(),
))]
fn connect(
    g: &GraphBuilder,
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
    realtime: bool,
    poll_mode: String,
) -> Result<(Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>)> {
    fix_dial(
        g,
        run_mode(realtime),
        &host,
        port,
        &sender_comp_id,
        &target_comp_id,
        self::poll_mode(&poll_mode)?,
    )
}

/// Bind a FIX acceptor on `port`, accepting one initiator connection.
///
/// Returns the same `(data, status)` tuple as [`fix_connect`], and takes the
/// same `poll_mode` / `realtime` arguments.
#[pyadapter(name = fix_accept, source)]
#[pyo3(signature = (
    port, sender_comp_id, target_comp_id,
    realtime = true, poll_mode = "threaded".to_string(),
))]
fn accept(
    g: &GraphBuilder,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
    realtime: bool,
    poll_mode: String,
) -> Result<(Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>)> {
    fix_bind(
        g,
        run_mode(realtime),
        port,
        &sender_comp_id,
        &target_comp_id,
        self::poll_mode(&poll_mode)?,
    )
}

/// Send each tick of this stream on a dedicated outbound FIX session.
///
/// Each tick's value is a message `dict` (`msg_type` + `fields`); `seq_num` is
/// stamped by the session, so it is ignored on the way out. The session
/// connects and logs on at graph `start()`, so an unreachable acceptor aborts
/// the run rather than raising at wiring, and a historical run aborts there
/// too. Returns a terminal stream whose value is `None`.
///
/// This has no legacy `wingfoil-python` equivalent.
#[pyadapter(name = fix_send)]
#[pyo3(signature = (host, port, sender_comp_id, target_comp_id))]
fn send(
    stream: &Stream<PyElement>,
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
) -> Result<Stream<()>> {
    let messages: Stream<FixMessage> =
        stream.try_map(|elem: &PyElement| element_to_message(elem, "fix_send"));
    messages.fix_send(&host, port, &sender_comp_id, &target_comp_id)
}

/// A live TLS FIX session: two streams plus a handle for sending.
///
/// Returned by [`fix_connect_tls`]. Hand-written rather than
/// `#[pyadapter]`-generated because the Rust adapter's [`FixConnection`] is a
/// stateful handle, not a plain stream tuple.
#[pyclass(name = "FixConnection", unsendable)]
pub struct PyFixConnection {
    connection: FixConnection,
    data: PyStream,
    status: PyStream,
}

#[pymethods]
impl PyFixConnection {
    /// Inbound application messages — ticks with a `list` of message dicts.
    #[getter]
    fn data(&self) -> crate::Stream {
        crate::Stream::from(self.data.clone())
    }

    /// Session lifecycle events — ticks with a `list` of status dicts.
    #[getter]
    fn status(&self) -> crate::Stream {
        crate::Stream::from(self.status.clone())
    }

    /// Queue an outbound message dict for sending on the session thread.
    ///
    /// Raises if the message is malformed, or if the send queue is full or the
    /// session has ended.
    fn send(&self, msg: &Bound<'_, PyDict>) -> PyResult<()> {
        let message = message_from_dict(msg, "FixConnection.send")
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))?;
        self.connection
            .send(message)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Subscribe to market data for the symbols arriving on `symbols`.
    ///
    /// Each tick of `symbols` is a `list` of symbol-ID strings; a
    /// `MarketDataRequest` (MsgType V) goes out for each one not yet
    /// subscribed. Symbols that arrive before logon are queued and sent once
    /// the session is ready, so wiring a `constant` works:
    ///
    /// ```python
    /// fix = fix_connect_tls(g, host, port, user, "LMXBD", password=pw)
    /// fix.fix_sub(g.constant(["4001", "4002"]))
    /// ```
    ///
    /// Returns a terminal stream whose value is `None`; it must stay wired into
    /// the graph for anything to be sent.
    fn fix_sub(&self, symbols: PyRef<'_, crate::Stream>) -> crate::Stream {
        let typed: Stream<Vec<String>> =
            symbols
                .object()
                .typed_input::<PyElement>()
                .try_map(|elem: &PyElement| {
                    if elem.is_none() {
                        bail!("fix_sub: stream value is empty (the upstream produced no value)");
                    }
                    Python::attach(|py| {
                        elem.object()
                            .bind(py)
                            .extract::<Vec<String>>()
                            .context("fix_sub: value must be a list of symbol-ID strings")
                    })
                });
        crate::Stream::from(
            symbols
                .object()
                .erased_output::<()>(self.connection.fix_sub(typed)),
        )
    }
}

/// Connect to a TLS-secured FIX acceptor (LMAX and similar venues).
///
/// `sender_comp_id` is usually the registered username; `password` is sent as
/// tag 554 in the Logon, with tag 553 set to `sender_comp_id`. Always uses the
/// threaded poll mode. Returns a [`FixConnection`](PyFixConnection) exposing
/// `.data`, `.status`, `.send(msg)` and `.fix_sub(symbols)`.
///
/// `realtime` must match the eventual `graph.run(realtime=…)`; `False` raises.
#[pyfunction]
#[pyo3(signature = (
    graph, host, port, sender_comp_id, target_comp_id, password = None, realtime = true,
))]
#[allow(clippy::too_many_arguments)]
pub fn fix_connect_tls(
    graph: PyRef<'_, Graph>,
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
    password: Option<String>,
    realtime: bool,
) -> PyResult<PyFixConnection> {
    let object = graph.object();
    let connection = fix_dial_tls(
        object.builder(),
        run_mode(realtime),
        &host,
        port,
        &sender_comp_id,
        &target_comp_id,
        password.as_deref(),
    )
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))?;
    Ok(PyFixConnection {
        data: object.erase_burst_source::<FixMessage>(connection.data.clone()),
        status: object.erase_burst_source::<FixSessionStatus>(connection.status.clone()),
        connection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::{PyList, PyTuple};

    fn message(fields: Vec<(u32, &str)>) -> FixMessage {
        FixMessage {
            msg_type: "W".into(),
            seq_num: 7,
            sending_time: wingfoil::NanoTime::ZERO,
            fields: fields
                .into_iter()
                .map(|(t, v)| (t, v.to_string()))
                .collect(),
        }
    }

    fn dict_element(py: Python<'_>, items: &[(&str, Py<PyAny>)]) -> PyElement {
        let dict = PyDict::new(py);
        for (k, v) in items {
            dict.set_item(k, v).unwrap();
        }
        PyElement::new(dict.into_any().unbind())
    }

    fn obj<'py, T: pyo3::IntoPyObjectExt<'py>>(py: Python<'py>, v: T) -> Py<PyAny> {
        v.into_py_any(py).unwrap()
    }

    #[test]
    fn a_message_erases_to_a_dict() {
        let element = PyElement::from(message(vec![(55, "EURUSD"), (270, "1.0842")]));
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("W", get("msg_type").extract::<String>().unwrap());
            assert_eq!(7, get("seq_num").extract::<u64>().unwrap());
            assert_eq!(
                vec![(55, "EURUSD".to_string()), (270, "1.0842".to_string())],
                get("fields").extract::<Vec<(u32, String)>>().unwrap()
            );
        });
    }

    #[test]
    fn every_status_erases_to_a_dict_with_a_status_key() {
        let cases = [
            (FixSessionStatus::Disconnected, "disconnected"),
            (FixSessionStatus::LoggingIn, "logging_in"),
            (FixSessionStatus::LoggedIn, "logged_in"),
            (FixSessionStatus::LoggedOut(None), "logged_out"),
            (FixSessionStatus::Error("boom".into()), "error"),
        ];
        Python::attach(|py| {
            for (status, expected) in cases {
                let element = PyElement::from(status);
                let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
                let name = dict.get_item("status").unwrap().unwrap();
                assert_eq!(expected, name.extract::<String>().unwrap());
            }
        });
    }

    #[test]
    fn a_carrying_status_keeps_its_payload() {
        Python::attach(|py| {
            let element = PyElement::from(FixSessionStatus::LoggedOut(Some("bye".into())));
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let reason = dict.get_item("reason").unwrap().unwrap();
            assert_eq!("bye", reason.extract::<String>().unwrap());

            let element = PyElement::from(FixSessionStatus::Error("boom".into()));
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let message = dict.get_item("message").unwrap().unwrap();
            assert_eq!("boom", message.extract::<String>().unwrap());
        });
    }

    #[test]
    fn a_logged_out_with_no_reason_carries_none() {
        Python::attach(|py| {
            let element = PyElement::from(FixSessionStatus::LoggedOut(None));
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            assert!(dict.get_item("reason").unwrap().unwrap().is_none());
        });
    }

    #[test]
    fn a_dict_marshals_to_a_message_with_tuple_pairs() {
        Python::attach(|py| {
            let pairs = PyList::new(
                py,
                [
                    PyTuple::new(py, [obj(py, 55_u32), obj(py, "EURUSD")]).unwrap(),
                    PyTuple::new(py, [obj(py, 262_u32), obj(py, "req1")]).unwrap(),
                ],
            )
            .unwrap();
            let element = dict_element(
                py,
                &[
                    ("msg_type", obj(py, "V")),
                    ("fields", obj(py, pairs)),
                    // Ignored: the session owns sequence numbering.
                    ("seq_num", obj(py, 99_u64)),
                ],
            );
            let msg = element_to_message(&element, "fix_send").unwrap();
            assert_eq!("V", msg.msg_type);
            assert_eq!(0, msg.seq_num);
            assert_eq!(
                vec![(55, "EURUSD".to_string()), (262, "req1".to_string())],
                msg.fields
            );
        });
    }

    #[test]
    fn list_pairs_marshal_too() {
        // The form legacy documented — `{"fields": [[262, "req1"]]}`.
        Python::attach(|py| {
            let pairs = PyList::new(
                py,
                [PyList::new(py, [obj(py, 262_u32), obj(py, "req1")]).unwrap()],
            )
            .unwrap();
            let element = dict_element(
                py,
                &[("msg_type", obj(py, "V")), ("fields", obj(py, pairs))],
            );
            let msg = element_to_message(&element, "fix_send").unwrap();
            assert_eq!(vec![(262, "req1".to_string())], msg.fields);
        });
    }

    #[test]
    fn a_missing_msg_type_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("fields", obj(py, Vec::<u32>::new()))]);
            let err = element_to_message(&element, "fix_send").unwrap_err();
            assert!(err.to_string().contains("missing 'msg_type'"), "{err}");
        });
    }

    #[test]
    fn a_missing_fields_key_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("msg_type", obj(py, "V"))]);
            let err = element_to_message(&element, "fix_send").unwrap_err();
            assert!(err.to_string().contains("missing 'fields'"), "{err}");
        });
    }

    #[test]
    fn a_wrong_length_pair_errors() {
        Python::attach(|py| {
            let pairs = PyList::new(py, [PyList::new(py, [obj(py, 262_u32)]).unwrap()]).unwrap();
            let element = dict_element(
                py,
                &[("msg_type", obj(py, "V")), ("fields", obj(py, pairs))],
            );
            let err = element_to_message(&element, "fix_send").unwrap_err();
            assert!(err.to_string().contains("got 1 items"), "{err}");
        });
    }

    #[test]
    fn a_non_str_tag_value_errors() {
        // FIX is a text protocol: a number must be spelled, never coerced.
        Python::attach(|py| {
            let pairs = PyList::new(
                py,
                [PyTuple::new(py, [obj(py, 270_u32), obj(py, 1.0842_f64)]).unwrap()],
            )
            .unwrap();
            let element = dict_element(
                py,
                &[("msg_type", obj(py, "V")), ("fields", obj(py, pairs))],
            );
            let err = element_to_message(&element, "fix_send").unwrap_err();
            assert!(err.to_string().contains("tag 270 must be a str"), "{err}");
        });
    }

    #[test]
    fn a_non_dict_value_errors() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_message(&element, "fix_send").unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "{err}");
        });
    }

    #[test]
    fn the_poll_mode_selector_is_a_string() {
        assert!(matches!(poll_mode("threaded"), Ok(FixPollMode::Threaded)));
        assert!(matches!(poll_mode("spin"), Ok(FixPollMode::AlwaysSpin)));
        // `FixPollMode` is not `Debug`, so match rather than `unwrap_err`.
        let Err(err) = poll_mode("polling") else {
            panic!("an unknown poll_mode must be rejected");
        };
        assert!(err.to_string().contains("expected 'threaded' or 'spin'"));
    }
}
