//! Python bindings for the wingfoil **ws** adapter
//! ([`wingfoil::adapters::ws`]) — a reconnecting WebSocket *client*.
//!
//! | Python | Rust | shape |
//! |---|---|---|
//! | `ws_sub(graph, url, …)` | [`ws_sub`](wingfoil::adapters::ws::ws_sub) | live frame source |
//! | `WsConnection(graph, url, …)` | [`ws_connect`](wingfoil::adapters::ws::ws_connect) | handle: `.messages`, `.status`, `.send()`, `.send_stream()` |
//!
//! Not to be confused with `WebServer` (the [`web`](super::web) binding), which
//! is the WebSocket *server* a browser connects to. This one dials out to
//! someone else's venue.
//!
//! # The dynamic edge
//!
//! A frame is a [`WsMessage`], which erases by variant: `Text` → Python `str`,
//! `Binary` → Python `bytes`. On the way back in, a `str` becomes `Text` and
//! `bytes`/`bytearray` becomes `Binary`; anything else is an error naming both
//! accepted types. That asymmetry is exact — unlike the `web` binding's
//! `bytes` → list-of-ints hop, nothing here is lost in either direction.
//!
//! Sources are burst-shaped, so **each tick yields a `list` of frames** — the
//! same-instant group, losslessly. Do not index `[0]` and assume that is all of
//! it; a busy venue routinely delivers several frames between graph cycles.
//!
//! A [`WsStatus`] erases to a **`dict`**: `{"state": "connected"}`,
//! `{"state": "reconnecting", "attempt": 3}`, `{"state": "failed"}`,
//! `{"state": "disconnected"}`.
//!
//! # `on_connect`
//!
//! Both `ws_sub` and `WsConnection` take `on_connect`, the Python face of
//! [`WsConfig::on_connect`]: a callable invoked once per successful connect,
//! reconnects included, whose result goes out after `subscriptions` and before
//! any frame queued on `WsConnection.send`. It is the escape hatch for a
//! subscription set that changes while the graph runs, or an auth frame that has
//! to be signed or timestamped for each attempt. It returns the payloads to
//! send — one `str`/`bytes`, or a `list`/`tuple` of them.
//!
//! This is the one place the binding holds a Python callable and runs it off the
//! graph thread, because the Rust adapter renders it on the connection task. One
//! `Python::attach` per connect buys that, not one per frame — the same acquire
//! every graph op already pays per value. A callable that blocks holds the GIL
//! for as long as it blocks and stalls the connect sequence, so build the
//! payload and return. [`WsConfig::on_connect`] is deliberately infallible
//! (`Fn() -> Vec<WsMessage>`, #971), so there is no failure channel to abort the
//! run through: an exception the callable raises is logged and that connect goes
//! out without the rendered payloads. The callable itself is checked at wiring.
//!
//! # Deviations
//!
//! There is no legacy `py_ws.rs` — the Rust adapter is wingfoil-only, so this
//! binding has no parity oracle. Two departures from the conventions the other
//! bindings set are deliberate:
//!
//! 1. **Status is a `dict`, not a string.** `aeron`'s `AeronStatus` erases to a
//!    plain string, and this would too, except that
//!    [`WsStatus::Reconnecting`] carries `attempt`. A string would silently
//!    drop it — and the retry count is the whole reason a caller watches the
//!    status stream rather than just noticing frames stopped.
//! 2. **`WsConnection` is a hand-written `#[pyclass]`, not `#[pyadapter]`.**
//!    `ws_connect` returns three things (frames, status, an outbound sender);
//!    `#[pyadapter]` emits one function with one return type and has no
//!    handle-receiver form. `ws_sub` — the common, frames-only case — *is*
//!    macro-generated, following the `fix` precedent that a binding may mix the
//!    two rather than hand-write the whole module.
//!
//! The run mode is an argument (`realtime`), as everywhere else: the Rust
//! factories reject a historical run at wiring, and a Python `Graph` does not
//! know its mode until `run()`.

use std::time::Duration;

use anyhow::{Result, bail};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use wingfoil::adapters::ws::{
    WsBackoff, WsConfig, WsMessage, WsSender, WsSinkOps, WsStatus, ws_connect as rust_ws_connect,
    ws_sub as rust_ws_sub,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::run_mode;
use crate::graph::PyStream;
use crate::{Graph, PyElement, pyadapter};

// ---------------------------------------------------------------------------
// The value edge
// ---------------------------------------------------------------------------

/// `Text` → `str`, `Binary` → `bytes`.
impl From<WsMessage> for PyElement {
    fn from(message: WsMessage) -> Self {
        match message {
            WsMessage::Text(text) => PyElement::from(text),
            WsMessage::Binary(bytes) => PyElement::from(bytes),
        }
    }
}

/// `str` → `Text`, `bytes`/`bytearray` → `Binary`; anything else is an error.
///
/// Loud rather than lenient: a caller passing a dict almost certainly meant to
/// `json.dumps` it, and silently sending its `repr` would reach the venue as a
/// malformed subscription that fails much later and much less clearly.
impl TryFrom<&PyElement> for WsMessage {
    type Error = anyhow::Error;

    fn try_from(element: &PyElement) -> Result<Self> {
        Python::attach(|py| {
            let value = element.object().bind(py);
            if let Ok(text) = value.extract::<String>() {
                return Ok(WsMessage::Text(text));
            }
            if let Ok(bytes) = value.extract::<Vec<u8>>() {
                return Ok(WsMessage::Binary(bytes));
            }
            bail!(
                "ws: a frame must be a str (text) or bytes (binary), got {}",
                value
                    .get_type()
                    .name()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|_| "?".to_string())
            )
        })
    }
}

/// A transition erases to a dict so `Reconnecting`'s `attempt` survives.
impl From<WsStatus> for PyElement {
    fn from(status: WsStatus) -> Self {
        // `WsStatus` is `#[non_exhaustive]`: a variant added upstream must not
        // silently become one of the four below, so it gets its own state name
        // rather than a default.
        let (state, attempt) = match status {
            WsStatus::Disconnected => ("disconnected".to_string(), None),
            WsStatus::Connected => ("connected".to_string(), None),
            WsStatus::Reconnecting { attempt } => ("reconnecting".to_string(), Some(attempt)),
            WsStatus::Failed => ("failed".to_string(), None),
            ref other => (format!("unknown:{other:?}"), None),
        };
        Python::attach(|py| {
            let dict = PyDict::new(py);
            let _ = dict.set_item("state", state);
            if let Some(attempt) = attempt {
                let _ = dict.set_item("attempt", attempt);
            }
            PyElement::from(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Build the Rust config from the flat argument list every entry point shares.
///
/// Kept in `anyhow` so argument validation reads like the other bindings'; the
/// callers map it to a Python exception.
#[allow(clippy::too_many_arguments)]
fn build_config(
    url: String,
    subscriptions: Option<Vec<Bound<'_, PyAny>>>,
    backoff_initial_secs: f64,
    backoff_max_secs: f64,
    backoff_multiplier: f64,
    jitter: bool,
    max_attempts: Option<u32>,
    idle_timeout_secs: Option<f64>,
    ping_interval_secs: Option<f64>,
    buffer_size: Option<usize>,
    on_connect: Option<Bound<'_, PyAny>>,
) -> Result<WsConfig> {
    let mut config = WsConfig::new(url).backoff(WsBackoff {
        initial: secs_to_duration("backoff_initial_secs", backoff_initial_secs)?,
        max: secs_to_duration("backoff_max_secs", backoff_max_secs)?,
        multiplier: backoff_multiplier,
        jitter,
        max_attempts,
    });

    for subscription in subscriptions.unwrap_or_default() {
        let element = PyElement::from(subscription);
        config = config.subscribe(WsMessage::try_from(&element)?);
    }
    if let Some(secs) = idle_timeout_secs {
        config = config.idle_timeout(secs_to_duration("idle_timeout_secs", secs)?);
    }
    if let Some(secs) = ping_interval_secs {
        config = config.ping_interval(secs_to_duration("ping_interval_secs", secs)?);
    }
    if let Some(frames) = buffer_size {
        config = config.buffer_size(frames);
    }
    if let Some(callback) = on_connect {
        // Validate at wiring so a typo (`on_connect="auth"`) names the argument
        // instead of surfacing as a logged miss on the first connect.
        if !callback.is_callable() {
            bail!(
                "ws: on_connect must be callable, got {}",
                callback
                    .get_type()
                    .name()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|_| "?".to_string())
            );
        }
        let callback = callback.unbind();
        config = config.on_connect(move || render_on_connect(&callback));
    }
    Ok(config)
}

/// Render a Python `on_connect` callable, on the connection task.
///
/// One [`Python::attach`] per connect — the same acquire the graph ops make
/// (`map`, `for_each`, …), reached here from the adapter's task instead of the
/// graph thread. The Rust `on_connect` closure is infallible by design (#971),
/// so a raised exception has nowhere to propagate to: it is logged and this
/// connect goes out without the rendered payloads. That is the narrow, once-per-
/// connect escape hatch; the callable itself is checked at wiring.
fn render_on_connect(callback: &Py<PyAny>) -> Vec<WsMessage> {
    Python::attach(|py| match callback.call0(py) {
        Err(err) => {
            log::error!("ws: on_connect raised, sending no rendered payloads: {err}");
            Vec::new()
        }
        Ok(rendered) => match frames_from_python(py, &rendered) {
            Ok(frames) => frames,
            Err(err) => {
                log::error!(
                    "ws: on_connect must return a frame or a list of frames, \
                     sending no rendered payloads: {err:#}"
                );
                Vec::new()
            }
        },
    })
}

/// A callable's return value as wire frames: one frame, or a `list`/`tuple` of
/// them — the same shape [`PyWsConnection::send_stream`] accepts on the way in.
///
/// `str` is a Python sequence, so the list check is explicit rather than a
/// `Vec` extraction that would split a text frame into characters.
fn frames_from_python(py: Python<'_>, rendered: &Py<PyAny>) -> Result<Vec<WsMessage>> {
    let value = rendered.bind(py);
    if value.is_instance_of::<PyList>() || value.is_instance_of::<PyTuple>() {
        let mut frames = Vec::new();
        for item in value.try_iter()? {
            let element = PyElement::from(item?);
            frames.push(WsMessage::try_from(&element)?);
        }
        return Ok(frames);
    }
    let element = PyElement::from(value.clone());
    Ok(vec![WsMessage::try_from(&element)?])
}

/// Seconds → [`Duration`], rejecting what would silently become nonsense.
///
/// `Duration::from_secs_f64` *panics* on a negative or non-finite value, which
/// would abort the interpreter rather than raise.
fn secs_to_duration(name: &str, secs: f64) -> Result<Duration> {
    if !secs.is_finite() || secs < 0.0 {
        bail!("ws: {name} must be a finite, non-negative number of seconds, got {secs}");
    }
    Ok(Duration::from_secs_f64(secs))
}

// ---------------------------------------------------------------------------
// The frames-only source
// ---------------------------------------------------------------------------

/// Stream frames from a WebSocket endpoint, reconnecting automatically.
///
/// Each tick yields a **`list`** of frames that arrived between graph cycles —
/// `str` for text frames, `bytes` for binary ones.
///
/// `subscriptions` are sent, in order, immediately after **every** connect,
/// including reconnects. That is the point of the adapter: a venue that hangs
/// up leaves a live socket carrying no subscriptions, so a feed that does not
/// re-send them goes silent while still looking connected. Put the subscribe
/// payload here rather than sending it once yourself.
///
/// A disconnect is not an error — it is a reconnect after
/// `backoff_initial_secs`, growing by `backoff_multiplier` up to
/// `backoff_max_secs`, with full jitter unless `jitter=False`. The run aborts
/// only if `max_attempts` is set and exhausted; the default (`None`) retries
/// forever, which is what a long-running process wants and what a test should
/// override.
///
/// `idle_timeout_secs` treats a connection with no frames for that long as
/// dead. Set it: venues routinely stop sending without closing the socket, and
/// nothing else notices. `ping_interval_secs` sends a keepalive ping.
///
/// `on_connect` is a callable rendered once per **successful** connect —
/// reconnects included — after `subscriptions` and before any frame is
/// delivered. It returns the payloads to send: one `str`/`bytes`, or a
/// `list`/`tuple` of them. Use it for a subscription set that changes while the
/// graph runs, or an auth frame that has to be signed or timestamped for each
/// attempt. It runs on the connection task with the GIL held, so keep it quick
/// and never block it on the graph: the socket is not read until it returns. An
/// exception it raises is logged and that connect sends no rendered payloads —
/// the Rust closure is infallible, so there is no failure channel to abort the
/// run through. A non-callable raises at wiring.
///
/// `realtime` must match the eventual `graph.run(...)`. A historical run raises
/// at wiring — a live socket has no timeline to replay. `wss://` works; the
/// wheel is built with TLS.
///
/// Use `WsConnection` instead if you need the connection status on the graph or
/// want to send frames out.
#[pyadapter(name = ws_sub, source)]
#[pyo3(signature = (
    url, subscriptions = None, realtime = true,
    backoff_initial_secs = 0.25, backoff_max_secs = 30.0, backoff_multiplier = 2.0,
    jitter = true, max_attempts = None,
    idle_timeout_secs = None, ping_interval_secs = None, buffer_size = None,
    on_connect = None,
))]
#[allow(clippy::too_many_arguments)]
fn sub(
    g: &GraphBuilder,
    url: String,
    subscriptions: Option<Vec<Bound<'_, PyAny>>>,
    realtime: bool,
    backoff_initial_secs: f64,
    backoff_max_secs: f64,
    backoff_multiplier: f64,
    jitter: bool,
    max_attempts: Option<u32>,
    idle_timeout_secs: Option<f64>,
    ping_interval_secs: Option<f64>,
    buffer_size: Option<usize>,
    on_connect: Option<Bound<'_, PyAny>>,
) -> Result<Stream<Burst<WsMessage>>> {
    let config = build_config(
        url,
        subscriptions,
        backoff_initial_secs,
        backoff_max_secs,
        backoff_multiplier,
        jitter,
        max_attempts,
        idle_timeout_secs,
        ping_interval_secs,
        buffer_size,
        on_connect,
    )?;
    rust_ws_sub(g, run_mode(realtime), config)
}

// ---------------------------------------------------------------------------
// The handle
// ---------------------------------------------------------------------------

/// A live WebSocket connection: frames, status, and an outbound channel.
///
/// ```python
/// g = wf.Graph()
/// conn = wf.WsConnection(
///     g,
///     "wss://stream.example.com/ws",
///     subscriptions=['{"op":"subscribe","args":["trades.BTC-USD"]}'],
///     idle_timeout_secs=20.0,
/// )
/// conn.messages.print()
/// conn.status.print()
/// g.run(realtime=True, duration_nanos=10_000_000_000)
/// ```
#[pyclass(name = "WsConnection", unsendable)]
pub struct PyWsConnection {
    messages: PyStream,
    status: PyStream,
    sender: WsSender,
}

#[pymethods]
impl PyWsConnection {
    /// Wire a reconnecting connection to `url` onto `graph`.
    ///
    /// Every argument means what it does on `ws_sub`, which see, `on_connect`
    /// included; this adds the status stream and the outbound half. Raises at
    /// wiring on a bad URL scheme, a historical run mode, a malformed
    /// subscription, or an `on_connect` that is not callable.
    #[new]
    #[pyo3(signature = (
        graph, url, subscriptions = None, realtime = true,
        backoff_initial_secs = 0.25, backoff_max_secs = 30.0, backoff_multiplier = 2.0,
        jitter = true, max_attempts = None,
        idle_timeout_secs = None, ping_interval_secs = None, buffer_size = None,
        on_connect = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        graph: PyRef<'_, Graph>,
        url: String,
        subscriptions: Option<Vec<Bound<'_, PyAny>>>,
        realtime: bool,
        backoff_initial_secs: f64,
        backoff_max_secs: f64,
        backoff_multiplier: f64,
        jitter: bool,
        max_attempts: Option<u32>,
        idle_timeout_secs: Option<f64>,
        ping_interval_secs: Option<f64>,
        buffer_size: Option<usize>,
        on_connect: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let config = build_config(
            url,
            subscriptions,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
            jitter,
            max_attempts,
            idle_timeout_secs,
            ping_interval_secs,
            buffer_size,
            on_connect,
        )
        .map_err(to_py_err)?;

        let object = graph.object();
        let connection =
            rust_ws_connect(object.builder(), run_mode(realtime), config).map_err(to_py_err)?;

        Ok(PyWsConnection {
            messages: object.erase_burst_source::<WsMessage>(connection.messages),
            status: object.erase_source::<WsStatus>(connection.status),
            sender: connection.sender,
        })
    }

    /// The frames, as a `list` of `str`/`bytes` per tick.
    #[getter]
    fn messages(&self) -> crate::Stream {
        crate::Stream::from(self.messages.clone())
    }

    /// The connection state, as a `dict` — ticking on transitions only.
    ///
    /// `{"state": "connected"}` once the socket is open *and* every
    /// subscription plus any `on_connect` payload has been sent;
    /// `{"state": "reconnecting", "attempt": n}` after a drop;
    /// `{"state": "failed"}` if `max_attempts` runs out, which also aborts the
    /// run.
    #[getter]
    fn status(&self) -> crate::Stream {
        crate::Stream::from(self.status.clone())
    }

    /// Queue one frame for delivery on this connection.
    ///
    /// Non-blocking: the socket write happens on the connection's own task.
    /// Callable before the graph runs — the frame is held and sent once the
    /// first connect has sent its `subscriptions` and any `on_connect`
    /// payloads. Anything that must survive a *reconnect* belongs in those
    /// instead; this queue replays a message once, not on every reconnect.
    ///
    /// Raises if the connection task has already ended.
    fn send(&self, message: &Bound<'_, PyAny>) -> PyResult<()> {
        let element = PyElement::from(message.clone());
        let frame = WsMessage::try_from(&element).map_err(to_py_err)?;
        self.sender.send(frame).map_err(to_py_err)
    }

    /// Send every value that ticks on `stream` out over this connection.
    ///
    /// Each tick may be a single frame or a `list`/`tuple` of them. Returns a
    /// terminal stream whose value is `None`; it must stay wired into the graph
    /// for anything to be sent.
    fn send_stream(&self, stream: PyRef<'_, crate::Stream>) -> PyResult<crate::Stream> {
        let object = stream.object();
        let frames: Stream<Burst<WsMessage>> =
            object
                .typed_burst_input::<PyElement>()
                .try_map(|burst: &Burst<PyElement>| {
                    // One attach for the whole burst; the per-element ones then
                    // short-circuit on the thread-local count.
                    Python::attach(|_py| burst.iter().map(WsMessage::try_from).collect())
                });
        let sink = frames.ws_send(&self.sender);
        Ok(crate::Stream::from(object.erased_output::<()>(sink)))
    }
}

fn to_py_err(e: anyhow::Error) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::{PyBytes, PyString};

    #[test]
    fn text_frames_erase_to_str_and_back() {
        Python::initialize();
        Python::attach(|py| {
            let element = PyElement::from(WsMessage::Text("hello".into()));
            let value = element.object().bind(py);
            assert_eq!(value.extract::<String>().expect("a str"), "hello");
            assert_eq!(
                WsMessage::try_from(&element).expect("round trip"),
                WsMessage::Text("hello".into())
            );
        });
    }

    #[test]
    fn binary_frames_erase_to_bytes_and_back() {
        Python::initialize();
        Python::attach(|py| {
            let element = PyElement::from(WsMessage::Binary(vec![1, 2, 3]));
            let value = element.object().bind(py);
            assert!(
                value.is_instance_of::<PyBytes>(),
                "a binary frame must reach Python as bytes, not a list of ints"
            );
            assert_eq!(
                WsMessage::try_from(&element).expect("round trip"),
                WsMessage::Binary(vec![1, 2, 3])
            );
        });
    }

    /// A `str` must not be mistaken for a byte sequence, nor the reverse — the
    /// two map to different WebSocket opcodes and venues do distinguish them.
    #[test]
    fn str_and_bytes_do_not_cross_over() {
        Python::initialize();
        Python::attach(|py| {
            let text = PyElement::from(PyString::new(py, "abc").into_any());
            assert_eq!(
                WsMessage::try_from(&text).expect("a str"),
                WsMessage::Text("abc".into())
            );
            let binary = PyElement::from(PyBytes::new(py, b"abc").into_any());
            assert_eq!(
                WsMessage::try_from(&binary).expect("bytes"),
                WsMessage::Binary(b"abc".to_vec())
            );
        });
    }

    #[test]
    fn an_unsupported_frame_type_names_both_accepted_types() {
        Python::initialize();
        Python::attach(|py| {
            let element = PyElement::from(PyDict::new(py).into_any());
            let err = WsMessage::try_from(&element).expect_err("a dict is not a frame");
            let message = format!("{err:#}");
            assert!(message.contains("str"), "unexpected error: {message}");
            assert!(message.contains("bytes"), "unexpected error: {message}");
        });
    }

    #[test]
    fn status_erases_to_a_dict_keeping_the_attempt() {
        Python::initialize();
        Python::attach(|py| {
            let element = PyElement::from(WsStatus::Reconnecting { attempt: 7 });
            let value = element.object().bind(py);
            let dict = value.cast::<PyDict>().expect("a dict");
            assert_eq!(
                dict.get_item("state")
                    .expect("state")
                    .expect("present")
                    .extract::<String>()
                    .expect("a str"),
                "reconnecting"
            );
            assert_eq!(
                dict.get_item("attempt")
                    .expect("attempt")
                    .expect("present")
                    .extract::<u32>()
                    .expect("an int"),
                7
            );
        });
    }

    #[test]
    fn simple_statuses_carry_no_attempt() {
        Python::initialize();
        Python::attach(|py| {
            for (status, name) in [
                (WsStatus::Connected, "connected"),
                (WsStatus::Disconnected, "disconnected"),
                (WsStatus::Failed, "failed"),
            ] {
                let element = PyElement::from(status);
                let value = element.object().bind(py);
                let dict = value.cast::<PyDict>().expect("a dict");
                assert_eq!(
                    dict.get_item("state")
                        .expect("state")
                        .expect("present")
                        .extract::<String>()
                        .expect("a str"),
                    name
                );
                assert!(
                    dict.get_item("attempt").expect("lookup").is_none(),
                    "{name} should carry no attempt"
                );
            }
        });
    }

    /// `Duration::from_secs_f64` panics on these, which would kill the
    /// interpreter instead of raising.
    #[test]
    fn non_finite_and_negative_durations_are_rejected() {
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let err = secs_to_duration("idle_timeout_secs", bad).expect_err("must be rejected");
            assert!(
                format!("{err:#}").contains("idle_timeout_secs"),
                "the error should name the argument: {err:#}"
            );
        }
    }

    #[test]
    fn valid_durations_convert() {
        assert_eq!(
            secs_to_duration("ping_interval_secs", 1.5).expect("valid"),
            Duration::from_millis(1500)
        );
        assert_eq!(
            secs_to_duration("backoff_initial_secs", 0.0).expect("zero is valid"),
            Duration::ZERO
        );
    }

    #[test]
    fn config_carries_subscriptions_in_order() {
        Python::initialize();
        Python::attach(|py| {
            let subscriptions = vec![
                PyString::new(py, "first").into_any(),
                PyString::new(py, "second").into_any(),
            ];
            let config = build_config(
                "wss://example.com/ws".into(),
                Some(subscriptions),
                0.25,
                30.0,
                2.0,
                true,
                Some(3),
                Some(5.0),
                None,
                None,
                None,
            )
            .expect("valid config");

            assert_eq!(
                config.subscriptions,
                vec![
                    WsMessage::Text("first".into()),
                    WsMessage::Text("second".into())
                ]
            );
            assert_eq!(config.backoff.max_attempts, Some(3));
            assert_eq!(config.idle_timeout, Some(Duration::from_secs(5)));
            assert_eq!(config.ping_interval, None);
        });
    }

    #[test]
    fn a_bad_subscription_fails_the_whole_config() {
        Python::initialize();
        Python::attach(|py| {
            let subscriptions = vec![PyDict::new(py).into_any()];
            let err = build_config(
                "wss://example.com/ws".into(),
                Some(subscriptions),
                0.25,
                30.0,
                2.0,
                true,
                None,
                None,
                None,
                None,
                None,
            )
            .expect_err("a dict is not a frame");
            assert!(format!("{err:#}").contains("str"), "{err:#}");
        });
    }

    /// The redaction the Rust adapter does must survive the binding — a Python
    /// traceback is exactly where a pasted URL with an embedded key ends up.
    #[test]
    fn config_redaction_survives_the_binding() {
        Python::initialize();
        Python::attach(|_py| {
            let config = build_config(
                "wss://key:secret123@example.com/ws?api_key=abc123".into(),
                None,
                0.25,
                30.0,
                2.0,
                true,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("valid config");
            let redacted = config.redacted();
            assert!(!redacted.contains("secret123"), "{redacted}");
            assert!(!redacted.contains("abc123"), "{redacted}");
        });
    }

    /// A config with one Python `on_connect` callable, defined from source so
    /// the test exercises a real Python function rather than a Rust stub the
    /// binding would never see.
    fn config_with_on_connect(source: &str) -> WsConfig {
        Python::attach(|py| {
            let source = std::ffi::CString::new(source).expect("no interior nul");
            let callback = py.eval(&source, None, None).expect("valid Python");
            build_config(
                "wss://example.com/ws".into(),
                None,
                0.25,
                30.0,
                2.0,
                true,
                None,
                None,
                None,
                None,
                Some(callback),
            )
            .expect("valid config")
        })
    }

    fn render(config: &WsConfig) -> Vec<WsMessage> {
        (config.on_connect.as_ref().expect("on_connect is set"))()
    }

    /// A typo in the argument should raise at wiring, not surface as a logged
    /// miss on the first connect.
    #[test]
    fn on_connect_must_be_callable() {
        Python::initialize();
        Python::attach(|py| {
            let err = build_config(
                "wss://example.com/ws".into(),
                None,
                0.25,
                30.0,
                2.0,
                true,
                None,
                None,
                None,
                None,
                Some(PyString::new(py, "auth").into_any()),
            )
            .expect_err("a str is not callable");
            let message = format!("{err:#}");
            assert!(message.contains("on_connect"), "{message}");
            assert!(message.contains("str"), "{message}");
        });
    }

    #[test]
    fn on_connect_renders_a_single_frame() {
        Python::initialize();
        let config = config_with_on_connect("lambda: 'auth'");
        assert_eq!(render(&config), vec![WsMessage::Text("auth".into())]);
    }

    /// `str` is a Python sequence, so a text frame must not be split into one
    /// frame per character.
    #[test]
    fn on_connect_does_not_split_a_text_frame() {
        Python::initialize();
        let config = config_with_on_connect("lambda: 'abc'");
        assert_eq!(render(&config), vec![WsMessage::Text("abc".into())]);
    }

    #[test]
    fn on_connect_renders_a_list_of_frames() {
        Python::initialize();
        let config = config_with_on_connect("lambda: ['auth', b'\\x01\\x02']");
        assert_eq!(
            render(&config),
            vec![
                WsMessage::Text("auth".into()),
                WsMessage::Binary(vec![1, 2]),
            ]
        );
    }

    /// The Rust closure is deliberately infallible (#971), so a raise or a
    /// non-frame return cannot abort the run: it sends nothing for that connect
    /// and is logged. Pinned so that is a decision rather than an accident.
    #[test]
    fn on_connect_renders_nothing_when_it_raises() {
        Python::initialize();
        let config = config_with_on_connect("lambda: 1 / 0");
        assert!(render(&config).is_empty());
    }

    #[test]
    fn on_connect_renders_nothing_for_a_non_frame_return() {
        Python::initialize();
        let config = config_with_on_connect("lambda: {'op': 'auth'}");
        assert!(render(&config).is_empty());
    }
}
