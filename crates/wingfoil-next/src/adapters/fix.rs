//! fix adapter — the FIX (Financial Information eXchange) protocol: a
//! synchronous, poll-based session engine (initiator + acceptor, plain TCP or
//! TLS). It ports the classic `wingfoil::adapters::fix` module onto the Op
//! model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `fix` feature. Bring in what you need explicitly:
//!
//! - **Sources** — the free functions [`fix_connect`] (initiator) and
//!   [`fix_accept`] (acceptor) on a [`GraphBuilder`]: both return the same
//!   `(Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>)` pair — a
//!   data stream of inbound application messages plus a status stream of session
//!   lifecycle transitions. [`fix_connect_tls`] / [`fix_connect_tls_logon`]
//!   connect to a TLS endpoint and return a [`FixConnection`] bundling those
//!   streams with a [`FixSender`] handle and the declarative
//!   [`fix_sub`](FixConnection::fix_sub) market-data subscription helper.
//! - **Sink** — the [`FixOperators`] extension trait on `Stream<FixMessage>`,
//!   enabled with `use wingfoil_next::adapters::fix::FixOperators;`: opens a
//!   dedicated outbound session and sends each message
//!   ([`fix_send`](FixOperators::fix_send)).
//!
//! # Poll modes (source machinery)
//!
//! FIX sessions are synchronous poll-based TCP connections, so — like the
//! classic adapter and deliberately *unlike* the async adapters (etcd, redis)
//! — this adapter uses no `async`/tokio runtime. Two poll modes trade latency
//! for CPU, selected by [`FixPollMode`]:
//!
//! - [`FixPollMode::AlwaysSpin`] — a busy-spin
//!   [`custom_node`](GraphBuilder::custom_node) drives non-blocking socket reads
//!   from the graph thread (~1–5 µs, one core pinned). No TLS.
//! - [`FixPollMode::Threaded`] — a background OS thread runs the session loop
//!   and feeds a [`channel`](crate::fluent::SourceOps::channel) (~10–100 µs,
//!   shares CPU). This is the mode `fix_connect_tls*` always use, and the only
//!   mode that reconnects after an established session drops (see below).
//!
//! Both modes are **realtime-only** and **reject
//! [`RunMode::HistoricalFrom`] at wiring time** with a "real-time" error: a live
//! FIX session has no historical timeline to replay, and the `Threaded` mode's
//! channel receiver would block-collect the never-closing stream up front and
//! deadlock at `start`. Run every FIX source under [`RunMode::RealTime`].
//!
//! Both modes multiplex data and session-status transitions in-band over one
//! transport (an internal [`FixEvent`] envelope), so a `LoggedIn` transition
//! stays correctly ordered relative to the messages around it; the streams are
//! split back apart before they reach the caller. Values arriving between graph
//! cycles group into one [`Burst`].
//!
//! # Session lifecycle & reconnect
//!
//! An initiator's socket connect and logon happen at graph **`start()`**
//! (deferred from wiring, matching the skill's live-source shape): a connection
//! failure surfaces when the run begins, with node context. On teardown the
//! session sends a best-effort Logout. In `Threaded` mode an initiator whose
//! *established* session drops **reconnects** (after a [`RECONNECT_DELAY`] pause
//! so a flapping venue isn't hammered) rather than giving up; acceptors loop to
//! re-accept. Initial connect *failures* still give up (an
//! [`FixSessionStatus::Error`] is emitted). `AlwaysSpin` initiators do not
//! reconnect. This matches classic exactly.
//!
//! # Sink
//!
//! [`FixOperators::fix_send`] opens its own outbound session (connect + logon at
//! graph `start()`, realtime-only) and writes each [`FixMessage`] from the graph
//! thread; back-pressure is the kernel TCP send buffer. A historical run aborts
//! at `start()` with a "real-time" error (matching classic's `start` check). The
//! [`FixSender`] handle (from [`FixConnection::sender`]) is the *other* outbound
//! path — a lock-free bounded queue drained by the `Threaded` session thread,
//! used for injecting messages (e.g. [`fix_sub`](FixConnection::fix_sub)'s
//! MarketDataRequests) into an established session from outside the graph.
//!
//! # Custom Logon authentication
//!
//! [`fix_connect_tls`] takes a `password: Option<&str>` (LMAX-style tag
//! 553/554). [`fix_connect_tls_logon`] takes a [`FixLogon`] for venues that
//! authenticate differently — [`FixLogon::custom`] hands a builder the
//! [`LogonContext`] (SenderCompID/TargetCompID/MsgSeqNum/SendingTime) so it can
//! attach a signature bound to the exact Logon header (e.g. Binance's Ed25519
//! `RawData`, tag 96, signed over tags 35/49/56/34/52 joined by SOH). wingfoil
//! stays free of venue/crypto specifics — the signer lives in the caller.
//!
//! # Deviations from classic
//!
//! Every classic *capability* is preserved — both poll modes, initiator and
//! acceptor, TLS, reconnect, the [`FixSender`] inject channel with its
//! [`SendError`] policy, [`fix_sub`](FixConnection::fix_sub), and
//! [`fix_send`](FixOperators::fix_send). The surface differs in the deliberate,
//! systemic ways every next adapter does:
//!
//! 1. **The source factories take a [`GraphBuilder`] and a [`RunMode`].** Every
//!    next source wires onto a builder, and a live source needs the run mode to
//!    reject `HistoricalFrom` at wiring (classic checked real-time-ness at run
//!    `start()`; next rejects earlier, at wiring). The message is the same
//!    ("real-time").
//! 2. **The sources return [`Stream`]s, not `Rc<dyn Stream>`;
//!    [`fix_send`](FixOperators::fix_send) returns `Result<Stream<()>>` and
//!    [`fix_sub`](FixConnection::fix_sub) a `Stream<()>`** (not `Rc<dyn Node>`) —
//!    the next stream/sink types. The socket connect + logon still happen at
//!    graph `start()` (like classic's `start`), the Logout at teardown (like
//!    `stop`).
//! 3. **No `AlwaysSpin` socket-shutdown fast-path in `Threaded` teardown.** The
//!    background session loop checks a stop flag against its 200 ms read timeout
//!    (the [`zmq`](crate::adapters::zmq) pattern) instead of classic's
//!    `Arc<Mutex<Option<TcpStream>>>` shutdown handle, so there is no lock on the
//!    graph path; teardown costs up to one read-timeout (200 ms) longer.
//!
//! The single-value convenience sink other adapters offer is not applicable
//! (the sink element is `FixMessage`, not a `Burst`). Everything else — the
//! codec, the session state machine, the field/tag semantics — is a verbatim
//! port. See also [`deviation-register.md`](../../../../docs/deviation-register.md).

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use tinyvec::TinyVec;
use wingfoil_next::RunMode;

use crate::Burst;
use crate::channel::ChannelSender;
use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::interp::StopHandle;
use crate::op::{Activation, Ctx, Tick};

// ── FIX constants ────────────────────────────────────────────────────────────

const TAG_BODY_LENGTH: u32 = 9;
const TAG_MSG_TYPE: u32 = 35;
const TAG_SENDER_COMP_ID: u32 = 49;
const TAG_TARGET_COMP_ID: u32 = 56;
const TAG_MSG_SEQ_NUM: u32 = 34;
const TAG_SENDING_TIME: u32 = 52;
const TAG_CHECKSUM: u32 = 10;
const TAG_BEGIN_STRING: u32 = 8;
const TAG_HEARTBT_INT: u32 = 108;
const TAG_TEST_REQ_ID: u32 = 112;
const TAG_ENCRYPT_METHOD: u32 = 98;
const TAG_USERNAME: u32 = 553;
const TAG_PASSWORD: u32 = 554;
const TAG_RESET_SEQ_NUM_FLAG: u32 = 141;
const TAG_TEXT: u32 = 58;

const MSG_HEARTBEAT: &str = "0";
const MSG_TEST_REQUEST: &str = "1";
const MSG_LOGON: &str = "A";
const MSG_LOGOUT: &str = "5";

const SOH: u8 = 0x01;
const BEGIN_STRING: &str = "FIX.4.4";
const HEARTBEAT_INTERVAL: u32 = 30;
const READ_BUF_SIZE: usize = 4096;

/// Pause before an initiator re-connects after an established session dropped, so
/// a flapping venue isn't hammered. (Connect *failures* still give up — this
/// covers only a session that logged in and later disconnected.)
pub const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Read timeout on the `Threaded` session socket: bounds how long the session
/// loop blocks in `read()` before checking the stop flag (and flushing the
/// inject queue) — the deferred-teardown budget in place of classic's socket
/// shutdown handle.
const THREADED_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Header/trailer tags excluded from the application-level `fields` list.
const HEADER_TAGS: &[u32] = &[
    TAG_BEGIN_STRING,
    TAG_BODY_LENGTH,
    TAG_MSG_TYPE,
    TAG_SENDER_COMP_ID,
    TAG_TARGET_COMP_ID,
    TAG_MSG_SEQ_NUM,
    TAG_SENDING_TIME,
    TAG_CHECKSUM,
];

// ── Public types ─────────────────────────────────────────────────────────────

/// A decoded FIX tag-value message.
#[derive(Debug, Clone, Default)]
pub struct FixMessage {
    /// MsgType (tag 35).
    pub msg_type: String,
    /// Inbound sequence number (tag 34).
    pub seq_num: u64,
    /// SendingTime as [`NanoTime`](wingfoil_next::NanoTime) (tag 52; currently set to
    /// zero — future work).
    pub sending_time: wingfoil_next::NanoTime,
    /// Application-level tag/value pairs (standard header and trailer excluded).
    pub fields: Vec<(u32, String)>,
}

impl FixMessage {
    /// Returns the value for `tag`, if present in the application fields.
    pub fn field(&self, tag: u32) -> Option<&str> {
        self.fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_str())
    }
}

/// FIX session lifecycle state.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum FixSessionStatus {
    #[default]
    Disconnected,
    LoggingIn,
    LoggedIn,
    /// Server sent a Logout (MsgType 5). Contains the `Text` field (tag 58) if present.
    LoggedOut(Option<String>),
    Error(String),
}

/// Internal event multiplexing data messages and session status changes over the
/// one transport. Split back into the `(data, status)` streams before it reaches
/// the caller, so a status transition stays correctly ordered relative to the
/// messages around it.
#[derive(Debug, Clone)]
pub enum FixEvent {
    Data(FixMessage),
    Status(FixSessionStatus),
}

impl Default for FixEvent {
    fn default() -> Self {
        FixEvent::Data(FixMessage::default())
    }
}

/// Controls how incoming FIX data is polled from the network.
pub enum FixPollMode {
    /// Graph spin loop drives polling — no dedicated thread, lowest latency.
    AlwaysSpin,
    /// Background thread + channel — shares CPU with other work.
    Threaded,
}

/// The subset of the Logon message the header is stamped with at send time,
/// handed to a [`FixLogon::Custom`] builder so it can compute an
/// authentication payload bound to the exact bytes going on the wire.
///
/// The canonical Binance/Ed25519 signing payload, for example, is the values of
/// tags 35, 49, 56, 34, 52 joined by SOH — every one of which is available here.
pub struct LogonContext<'a> {
    /// MsgType (tag 35) — always `"A"` for a Logon.
    pub msg_type: &'a str,
    /// SenderCompID (tag 49).
    pub sender_comp_id: &'a str,
    /// TargetCompID (tag 56).
    pub target_comp_id: &'a str,
    /// MsgSeqNum (tag 34) this Logon will carry.
    pub msg_seq_num: u64,
    /// SendingTime (tag 52) this Logon will carry, exactly as formatted on the wire.
    pub sending_time: &'a str,
}

/// How a session authenticates in its Logon message.
///
/// The default `None` sends only EncryptMethod/HeartBtInt/ResetSeqNumFlag.
/// `Password` adds Username (tag 553 = SenderCompID) and Password (tag 554),
/// as LMAX and similar venues expect. `Custom` hands a builder the
/// [`LogonContext`] and appends whatever tag/value pairs it returns — the seam
/// venues like Binance use to attach an Ed25519 signature (RawData, tag 96)
/// computed over the logon header.
#[derive(Clone)]
pub enum FixLogon {
    /// No authentication fields.
    None,
    /// Username (tag 553 = SenderCompID) + Password (tag 554).
    Password(String),
    /// Caller-supplied builder, e.g. an Ed25519 RawData signature.
    Custom(Arc<dyn Fn(&LogonContext) -> Vec<(u32, String)> + Send + Sync>),
}

impl FixLogon {
    /// Wrap a logon-field builder (see [`FixLogon::Custom`]).
    pub fn custom(
        builder: impl Fn(&LogonContext) -> Vec<(u32, String)> + Send + Sync + 'static,
    ) -> Self {
        FixLogon::Custom(Arc::new(builder))
    }
}

impl From<Option<&str>> for FixLogon {
    fn from(password: Option<&str>) -> Self {
        match password {
            Some(p) => FixLogon::Password(p.to_string()),
            None => FixLogon::None,
        }
    }
}

/// Bounded capacity of the outbound inject channel. `try_send` returns full
/// when this many messages are backlogged; callers receive
/// [`SendError::QueueFull`] and decide the policy (halt, retry, log-and-drop,
/// etc.).
const INJECT_QUEUE_CAPACITY: usize = 1024;

/// Reasons [`FixSender::send`] can fail.
///
/// Both variants mean the message was not queued. `QueueFull` is transient —
/// the session thread is stalled but may recover. `Closed` is terminal — the
/// session has ended and all subsequent sends will also return `Closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The bounded send queue is full ({INJECT_QUEUE_CAPACITY} messages).
    /// The session thread is not draining fast enough, typically because the
    /// socket is backpressured.
    QueueFull,
    /// The inject channel is closed — the session thread has exited.
    Closed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::QueueFull => {
                write!(f, "FIX send queue full ({INJECT_QUEUE_CAPACITY} messages)")
            }
            SendError::Closed => f.write_str("FIX send channel closed"),
        }
    }
}

impl std::error::Error for SendError {}

/// Handle for sending outbound [`FixMessage`]s on an established FIX session.
///
/// Obtained from [`FixConnection::sender`]. Thread-safe and cheaply cloneable.
/// Messages are sent on the next background-thread loop iteration.
///
/// Internally backed by a lock-free bounded [`kanal`] MPSC channel: `send()`
/// does a single CAS-style `try_send` and never blocks. Failure (full or
/// closed) is surfaced via [`SendError`] so callers choose the policy:
/// halt trading, route to a dead-letter queue, retry, or log-and-drop via
/// [`FixSender::send_or_log`].
#[derive(Clone)]
pub struct FixSender {
    sender: kanal::Sender<FixMessage>,
}

impl FixSender {
    /// Queue `msg` for sending on the next session loop iteration.
    ///
    /// Non-blocking. Returns [`SendError::QueueFull`] if the session thread is
    /// stalled and the bounded channel is full, or [`SendError::Closed`] if
    /// the session has ended. The message is not queued in either case.
    pub fn send(&self, msg: FixMessage) -> Result<(), SendError> {
        match self.sender.try_send(msg) {
            Ok(true) => Ok(()),
            Ok(false) => Err(SendError::QueueFull),
            Err(_) => Err(SendError::Closed),
        }
    }

    /// Convenience wrapper around [`send`](Self::send) that logs and drops on
    /// failure. Appropriate for non-critical paths (e.g. market-data
    /// subscriptions that are idempotent and resendable). For order routing or
    /// other paths where silent drops are unacceptable, use [`send`](Self::send)
    /// directly and handle the [`SendError`].
    pub fn send_or_log(&self, msg: FixMessage) {
        if let Err(e) = self.send(msg) {
            log::warn!("FixSender: {e} — dropping message");
        }
    }
}

/// Bundles the streams and session handle returned by [`fix_connect_tls`].
///
/// Use [`fix_sub`](FixConnection::fix_sub) to subscribe to market data as a
/// graph node, or [`send`](FixConnection::send) for raw outbound messages.
pub struct FixConnection {
    /// Inbound application messages (MarketDataSnapshot, execution reports, etc.).
    pub data: Stream<Burst<FixMessage>>,
    /// Session lifecycle events (LoggedIn, LoggedOut, …).
    pub status: Stream<Burst<FixSessionStatus>>,
    graph: GraphBuilder,
    sender: FixSender,
}

impl FixConnection {
    /// Create a graph node that subscribes to market data for symbols arriving
    /// on `symbols`.
    ///
    /// The node watches both the symbol stream (active) and the session status
    /// stream (active). When a new batch of symbol IDs arrives and the session
    /// is [`FixSessionStatus::LoggedIn`], a `MarketDataRequest` (MsgType V) is
    /// sent for each unseen symbol. Symbols that arrive before logon are queued
    /// and subscribed automatically once the session is ready.
    ///
    /// For a fixed set of symbols, use
    /// [`constant`](crate::fluent::SourceOps::constant):
    ///
    /// ```ignore
    /// let fix = fix_connect_tls(&g, RunMode::RealTime, host, port, sender, target, Some(&pw))?;
    /// let sub = fix.fix_sub(g.constant(vec!["4001".into(), "4002".into()]));
    /// ```
    pub fn fix_sub(&self, symbols: Stream<Vec<String>>) -> Stream<()> {
        let status_slot = self.status.value_slot();
        let symbols_slot = symbols.value_slot();
        let mut state = FixSubState {
            sender: self.sender.clone(),
            pending: Vec::new(),
            sent: Vec::new(),
            logged_in: false,
        };
        self.graph.custom_node::<(), _>(
            &[self.status.upstream(), symbols.upstream()],
            &[],
            Activation::NONE,
            move |_ctx| {
                // Detect LoggedIn on the status stream.
                if status_slot
                    .borrow()
                    .iter()
                    .any(|s| matches!(s, FixSessionStatus::LoggedIn))
                {
                    state.logged_in = true;
                }

                // Collect new symbols from the stream (cloned so the slot borrow
                // is released before we mutate `state`).
                let symbols: Vec<String> = symbols_slot.borrow().clone();
                for sym in symbols {
                    if !state.sent.contains(&sym) && !state.pending.contains(&sym) {
                        if state.logged_in {
                            state.subscribe(&sym);
                        } else {
                            state.pending.push(sym);
                        }
                    }
                }

                // Drain anything that was pending before logon.
                if state.logged_in && !state.pending.is_empty() {
                    for sym in std::mem::take(&mut state.pending) {
                        if !state.sent.contains(&sym) {
                            state.subscribe(&sym);
                        }
                    }
                }

                Ok(Tick::Quiet) // sink — never ticks downstream
            },
        )
    }

    /// Queue a raw outbound [`FixMessage`] for sending on the session thread.
    ///
    /// See [`FixSender::send`] for error semantics.
    pub fn send(&self, msg: FixMessage) -> Result<(), SendError> {
        self.sender.send(msg)
    }

    /// Get a clone of the underlying [`FixSender`] for manual use.
    pub fn sender(&self) -> FixSender {
        self.sender.clone()
    }
}

/// Per-node state of the market-data subscription sink (see
/// [`FixConnection::fix_sub`]).
struct FixSubState {
    sender: FixSender,
    pending: Vec<String>,
    sent: Vec<String>,
    logged_in: bool,
}

impl FixSubState {
    fn subscribe(&mut self, sym: &str) {
        let req_id = format!("sub_{}_{sym}", self.sent.len());
        self.sender.send_or_log(market_data_request(sym, &req_id));
        self.sent.push(sym.to_string());
    }
}

// ── FIX tag-value codec ───────────────────────────────────────────────────────

fn append_field(buf: &mut Vec<u8>, tag: u32, value: &str) {
    buf.extend_from_slice(tag.to_string().as_bytes());
    buf.push(b'=');
    buf.extend_from_slice(value.as_bytes());
    buf.push(SOH);
}

/// Current UTC time formatted as a FIX `SendingTime` (tag 52) with millisecond
/// precision (`YYYYMMDD-HH:MM:SS.sss`). Millisecond precision is what venues
/// like Binance require, and it is the exact string a Logon signature is
/// computed over, so it is formatted once and threaded through both.
fn now_sending_time() -> String {
    chrono::Utc::now().format("%Y%m%d-%H:%M:%S%.3f").to_string()
}

fn encode_message(
    msg_type: &str,
    sender: &str,
    target: &str,
    seq: u64,
    sending_time: &str,
    extra: &[(u32, String)],
) -> Vec<u8> {
    let mut body = Vec::<u8>::new();
    append_field(&mut body, TAG_MSG_TYPE, msg_type);
    append_field(&mut body, TAG_SENDER_COMP_ID, sender);
    append_field(&mut body, TAG_TARGET_COMP_ID, target);
    append_field(&mut body, TAG_MSG_SEQ_NUM, &seq.to_string());
    append_field(&mut body, TAG_SENDING_TIME, sending_time);
    for (tag, val) in extra {
        append_field(&mut body, *tag, val);
    }

    let mut out = Vec::<u8>::new();
    append_field(&mut out, TAG_BEGIN_STRING, BEGIN_STRING);
    append_field(&mut out, TAG_BODY_LENGTH, &body.len().to_string());
    out.extend_from_slice(&body);
    let checksum: u8 = out.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    append_field(&mut out, TAG_CHECKSUM, &format!("{checksum:03}"));
    out
}

fn decode_fields(data: &[u8]) -> Vec<(u32, String)> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let Some(eq_off) = data[pos..].iter().position(|&b| b == b'=') else {
            break;
        };
        let eq = pos + eq_off;
        let tag: u32 = match std::str::from_utf8(&data[pos..eq])
            .ok()
            .and_then(|s| s.parse().ok())
        {
            Some(t) => t,
            None => {
                pos = eq + 1;
                continue;
            }
        };
        let Some(soh_off) = data[eq + 1..].iter().position(|&b| b == SOH) else {
            break;
        };
        let soh = eq + 1 + soh_off;
        let value = std::str::from_utf8(&data[eq + 1..soh])
            .unwrap_or("")
            .to_string();
        fields.push((tag, value));
        pos = soh + 1;
    }
    fields
}

fn build_message(all: Vec<(u32, String)>) -> Option<FixMessage> {
    let msg_type = all.iter().find(|(t, _)| *t == TAG_MSG_TYPE)?.1.clone();
    let seq_num = all
        .iter()
        .find(|(t, _)| *t == TAG_MSG_SEQ_NUM)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let fields = all
        .into_iter()
        .filter(|(t, _)| !HEADER_TAGS.contains(t))
        .collect();
    Some(FixMessage {
        msg_type,
        seq_num,
        sending_time: wingfoil_next::NanoTime::ZERO,
        fields,
    })
}

/// Find the first complete FIX message in `buf` (delimited by `\x0110=xxx\x01`).
/// Returns `(owned_msg_bytes, bytes_consumed)`.
fn find_message(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    let pattern = b"\x0110=";
    let pos = buf.windows(pattern.len()).position(|w| w == pattern)?;
    let val_start = pos + pattern.len();
    let soh_off = buf[val_start..].iter().position(|&b| b == SOH)?;
    let end = val_start + soh_off + 1;
    Some((buf[..end].to_vec(), end))
}

/// Drain all complete FIX messages from `parse_buf`, dispatching session-level messages
/// and pushing application/status events into `events`.
/// Returns `true` if any events were pushed.
fn drain_parse_buf<W: Write>(
    parse_buf: &mut Vec<u8>,
    socket: &mut Option<W>,
    session: &mut FixSession,
    events: &mut Burst<FixEvent>,
    is_acceptor: bool,
) -> anyhow::Result<bool> {
    let before = events.len();
    while let Some((msg_bytes, consumed)) = find_message(parse_buf) {
        parse_buf.drain(..consumed);
        let Some(msg) = build_message(decode_fields(&msg_bytes)) else {
            continue;
        };
        let mut sock = match socket.take() {
            Some(s) => s,
            None => continue,
        };
        let pass = if is_acceptor {
            handle_acceptor(session, &msg, &mut sock, events)?
        } else {
            handle_initiator(session, &msg, &mut sock, events)?
        };
        *socket = Some(sock);
        if pass {
            events.push(FixEvent::Data(msg));
        }
    }
    Ok(events.len() > before)
}

// ── FixSession ────────────────────────────────────────────────────────────────

struct FixSession {
    sender_comp_id: String,
    target_comp_id: String,
    out_seq: u64,
    /// How the Logon message authenticates (password, custom signer, or none).
    logon: FixLogon,
}

impl FixSession {
    fn new(sender: &str, target: &str) -> Self {
        Self::new_with_logon(sender, target, FixLogon::None)
    }

    fn new_with_logon(sender: &str, target: &str, logon: FixLogon) -> Self {
        Self {
            sender_comp_id: sender.to_string(),
            target_comp_id: target.to_string(),
            out_seq: 0,
            logon,
        }
    }

    /// Encode `msg_type` with the current sequence number and `sending_time`,
    /// then write and flush it. Shared by [`send`](Self::send) and
    /// [`send_with`](Self::send_with) so both stamp the frame identically.
    fn write_encoded<W: Write>(
        &mut self,
        sock: &mut W,
        msg_type: &str,
        sending_time: &str,
        extra: &[(u32, String)],
    ) -> anyhow::Result<()> {
        let bytes = encode_message(
            msg_type,
            &self.sender_comp_id,
            &self.target_comp_id,
            self.out_seq,
            sending_time,
            extra,
        );
        sock.write_all(&bytes)?;
        sock.flush()?;
        Ok(())
    }

    /// Stamp and send a message, building the application fields from the
    /// sequence number and SendingTime this message will carry. The two are
    /// computed once here so a signer sees exactly what goes on the wire.
    fn send_with<W: Write>(
        &mut self,
        sock: &mut W,
        msg_type: &str,
        build_extra: impl FnOnce(u64, &str) -> Vec<(u32, String)>,
    ) -> anyhow::Result<()> {
        self.out_seq += 1;
        let sending_time = now_sending_time();
        let extra = build_extra(self.out_seq, &sending_time);
        self.write_encoded(sock, msg_type, &sending_time, &extra)
    }

    fn send<W: Write>(
        &mut self,
        sock: &mut W,
        msg_type: &str,
        extra: &[(u32, String)],
    ) -> anyhow::Result<()> {
        self.out_seq += 1;
        let sending_time = now_sending_time();
        self.write_encoded(sock, msg_type, &sending_time, extra)
    }

    fn send_logon<W: Write>(&mut self, sock: &mut W) -> anyhow::Result<()> {
        let logon = self.logon.clone();
        let sender_id = self.sender_comp_id.clone();
        let target_id = self.target_comp_id.clone();
        self.send_with(sock, MSG_LOGON, move |seq, sending_time| {
            let mut extra = vec![
                (TAG_ENCRYPT_METHOD, "0".to_string()),
                (TAG_HEARTBT_INT, HEARTBEAT_INTERVAL.to_string()),
                // ResetSeqNumFlag=Y tells the counterparty to reset sequence
                // numbers, avoiding rejections due to stale expected sequence
                // numbers from previous sessions.
                (TAG_RESET_SEQ_NUM_FLAG, "Y".to_string()),
            ];
            match &logon {
                FixLogon::None => {}
                FixLogon::Password(pwd) => {
                    // LMAX and other venues require tag 553 (Username) =
                    // SenderCompID and tag 554 (Password) in the Logon message.
                    extra.push((TAG_USERNAME, sender_id.clone()));
                    extra.push((TAG_PASSWORD, pwd.clone()));
                }
                FixLogon::Custom(builder) => {
                    let ctx = LogonContext {
                        msg_type: MSG_LOGON,
                        sender_comp_id: &sender_id,
                        target_comp_id: &target_id,
                        msg_seq_num: seq,
                        sending_time,
                    };
                    extra.extend(builder(&ctx));
                }
            }
            extra
        })
    }

    fn send_logout<W: Write>(&mut self, sock: &mut W) -> anyhow::Result<()> {
        self.send(sock, MSG_LOGOUT, &[])
    }

    fn send_heartbeat<W: Write>(
        &mut self,
        sock: &mut W,
        test_req_id: Option<String>,
    ) -> anyhow::Result<()> {
        let extra = test_req_id
            .map(|id| vec![(TAG_TEST_REQ_ID, id)])
            .unwrap_or_default();
        self.send(sock, MSG_HEARTBEAT, &extra)
    }
}

/// Handle a session-level message for the **initiator** role.
/// Appends any generated status events to `events`.
/// Returns `true` if the message should be forwarded to the application layer.
fn handle_initiator<W: Write>(
    session: &mut FixSession,
    msg: &FixMessage,
    sock: &mut W,
    events: &mut Burst<FixEvent>,
) -> anyhow::Result<bool> {
    match msg.msg_type.as_str() {
        MSG_LOGON => {
            events.push(FixEvent::Status(FixSessionStatus::LoggedIn));
            Ok(false)
        }
        MSG_HEARTBEAT => Ok(false),
        MSG_TEST_REQUEST => {
            let id = msg.field(TAG_TEST_REQ_ID).map(str::to_string);
            session.send_heartbeat(sock, id)?;
            Ok(false)
        }
        MSG_LOGOUT => {
            let reason = msg.field(TAG_TEXT).map(str::to_string);
            events.push(FixEvent::Status(FixSessionStatus::LoggedOut(reason)));
            Ok(false)
        }
        _ => Ok(true),
    }
}

/// Handle a session-level message for the **acceptor** role.
fn handle_acceptor<W: Write>(
    session: &mut FixSession,
    msg: &FixMessage,
    sock: &mut W,
    events: &mut Burst<FixEvent>,
) -> anyhow::Result<bool> {
    match msg.msg_type.as_str() {
        MSG_LOGON => {
            session.send_logon(sock)?;
            events.push(FixEvent::Status(FixSessionStatus::LoggedIn));
            Ok(false)
        }
        MSG_HEARTBEAT => Ok(false),
        MSG_TEST_REQUEST => {
            let id = msg.field(TAG_TEST_REQ_ID).map(str::to_string);
            session.send_heartbeat(sock, id)?;
            Ok(false)
        }
        MSG_LOGOUT => {
            let reason = msg.field(TAG_TEXT).map(str::to_string);
            events.push(FixEvent::Status(FixSessionStatus::LoggedOut(reason)));
            Ok(false)
        }
        _ => Ok(true),
    }
}

// ── Shared connection helpers ──────────────────────────────────────────────────

fn connect_with_retry(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    for attempt in 0..20u32 {
        match TcpStream::connect((host, port)) {
            Ok(s) => return Ok(s),
            Err(_) if attempt < 19 => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("failed to connect to {host}:{port} after 20 attempts")
}

/// Wrap a [`TcpStream`] in a TLS client connection targeting `host`.
///
/// Uses the Mozilla root CA bundle via `webpki-roots`. The TLS handshake is
/// deferred until the first read/write on the returned stream.
fn tls_connect(
    host: &str,
    stream: TcpStream,
) -> anyhow::Result<StreamOwned<ClientConnection, TcpStream>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = Arc::new(
        ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );

    let server_name: ServerName<'static> = host
        .to_string()
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid TLS server name: {host}"))?;

    let conn = ClientConnection::new(config, server_name)?;
    Ok(StreamOwned::new(conn, stream))
}

/// Split a multiplexed [`FixEvent`] stream into the `(data, status)` pair — the
/// next twin of classic's `split_events`. Each half keeps the burst grouping:
/// same-instant messages/statuses ride one burst, and the half only ticks when
/// its projection is non-empty.
fn split_events(
    events: Stream<Burst<FixEvent>>,
) -> (Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>) {
    let data = events.map_filter(|burst: &Burst<FixEvent>| {
        let msgs: Burst<FixMessage> = burst
            .iter()
            .filter_map(|e| match e {
                FixEvent::Data(m) => Some(m.clone()),
                FixEvent::Status(_) => None,
            })
            .collect();
        let ticked = !msgs.is_empty();
        (msgs, ticked)
    });

    let status = events.map_filter(|burst: &Burst<FixEvent>| {
        let statuses: Burst<FixSessionStatus> = burst
            .iter()
            .filter_map(|e| match e {
                FixEvent::Status(s) => Some(s.clone()),
                FixEvent::Data(_) => None,
            })
            .collect();
        let ticked = !statuses.is_empty();
        (statuses, ticked)
    });

    (data, status)
}

/// Reject a historical run at wiring time: a live FIX session has no historical
/// timeline to replay (and the `Threaded` mode's channel receiver would
/// block-collect the never-closing stream and deadlock at `start`). Matches
/// classic's run-`start()` "real-time" check, moved earlier to wiring.
fn reject_historical(run_mode: RunMode) -> Result<()> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "FIX sources only support real-time mode — RunMode::HistoricalFrom \
             is unsupported (a live session has no historical timeline to replay); \
             run under RunMode::RealTime"
        );
    }
    Ok(())
}

/// Immutable session configuration shared by both poll modes.
#[derive(Clone)]
struct FixConfig {
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
    logon: FixLogon,
    tls: bool,
    is_acceptor: bool,
}

// ── AlwaysSpin source (busy-spin custom node) ──────────────────────────────────

/// Graph-thread-local state of an `AlwaysSpin` session. Connected/bound at graph
/// `start()`, polled non-blocking each cycle, torn down (Logout) at teardown.
struct SpinState {
    is_acceptor: bool,
    session: FixSession,
    socket: Option<TcpStream>,
    listener: Option<TcpListener>,
    parse_buf: Vec<u8>,
    /// Set by `start()` on an initiator so the first cycle emits `LoggingIn`
    /// (classic emits it from `start`, which the next start hook cannot).
    logging_in_pending: bool,
}

impl SpinState {
    fn new(cfg: &FixConfig) -> Self {
        Self {
            is_acceptor: cfg.is_acceptor,
            session: FixSession::new(&cfg.sender_comp_id, &cfg.target_comp_id),
            socket: None,
            listener: None,
            parse_buf: Vec::new(),
            logging_in_pending: false,
        }
    }

    /// One busy-spin cycle: accept (acceptor), read non-blocking, and drain
    /// complete messages, collecting the cycle's events.
    fn cycle(&mut self) -> anyhow::Result<Burst<FixEvent>> {
        let mut events: Burst<FixEvent> = TinyVec::new();

        if self.logging_in_pending {
            events.push(FixEvent::Status(FixSessionStatus::LoggingIn));
            self.logging_in_pending = false;
        }

        // Accept phase (acceptor only).
        if self.is_acceptor
            && self.socket.is_none()
            && let Some(listener) = self.listener.as_ref()
        {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true)?;
                    self.socket = Some(stream);
                    events.push(FixEvent::Status(FixSessionStatus::LoggingIn));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }

        // Read phase.
        let mut eof = false;
        if let Some(sock) = self.socket.as_mut() {
            let mut tmp = [0u8; READ_BUF_SIZE];
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => self.parse_buf.extend_from_slice(&tmp[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }

        if eof {
            self.socket = None;
            events.push(FixEvent::Status(FixSessionStatus::Disconnected));
            return Ok(events);
        }

        drain_parse_buf(
            &mut self.parse_buf,
            &mut self.socket,
            &mut self.session,
            &mut events,
            self.is_acceptor,
        )?;
        Ok(events)
    }
}

/// Drops the `AlwaysSpin` session cleanly at teardown: send a best-effort Logout
/// on the live socket (classic's `stop`).
struct SpinLogoutGuard(Rc<RefCell<SpinState>>);

impl Drop for SpinLogoutGuard {
    fn drop(&mut self) {
        let mut st = self.0.borrow_mut();
        let sock = st.socket.take();
        if let Some(mut sock) = sock {
            let _ = st.session.send_logout(&mut sock);
        }
    }
}

/// Wire an `AlwaysSpin` session as a busy-spin [`custom_node`](GraphBuilder::custom_node):
/// the connect/bind + Logout lifecycle is deferred to graph `start()`/teardown,
/// the non-blocking read runs every cycle.
fn spin_source(g: &GraphBuilder, cfg: FixConfig) -> Stream<Burst<FixEvent>> {
    let state = Rc::new(RefCell::new(SpinState::new(&cfg)));
    let cycle_state = state.clone();

    let events = g.custom_node::<Burst<FixEvent>, _>(&[], &[], Activation::ALWAYS, move |_ctx| {
        let evs = cycle_state.borrow_mut().cycle()?;
        Ok(if evs.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(evs)
        })
    });

    let idx = events.handle().index();
    let start_state = state;
    g.with_builder(move |b| {
        b.compose_spawn_at_start(idx, move |_run_mode, _run_for, _start_time| {
            // Connect (initiator) or bind (acceptor); a connect failure aborts the
            // run at start with context — classic's `start` `?`.
            {
                let mut st = start_state.borrow_mut();
                if st.is_acceptor {
                    let listener = TcpListener::bind(("0.0.0.0", cfg.port))
                        .with_context(|| format!("fix_accept: binding port {}", cfg.port))?;
                    listener.set_nonblocking(true)?;
                    st.listener = Some(listener);
                } else {
                    let mut sock = connect_with_retry(&cfg.host, cfg.port).with_context(|| {
                        format!("fix_connect: connecting to {}:{}", cfg.host, cfg.port)
                    })?;
                    st.session.send_logon(&mut sock)?;
                    sock.set_nonblocking(true)?;
                    st.socket = Some(sock);
                    st.logging_in_pending = true;
                }
            }
            Ok(StopHandle::new(SpinLogoutGuard(start_state.clone())))
        });
    });

    events
}

// ── Threaded source (background session thread over the channel) ───────────────

/// Signals the background session thread to stop at teardown (sets the stop
/// flag) and joins it. No socket-shutdown handle: the session loop's read has a
/// [`THREADED_READ_TIMEOUT`], so it observes the flag on its next timeout — the
/// zmq-adapter pattern, no lock on the graph path.
struct ThreadedStopGuard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ThreadedStopGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

/// Wire a `Threaded` session: the socket connect + session thread are deferred
/// to graph `start()` via [`source_at_start`](crate::fluent::SourceOps::source_at_start).
/// Returns the multiplexed event stream plus the [`FixSender`] for the outbound
/// inject channel (created at wiring so the handle is usable before the run).
fn threaded_source(g: &GraphBuilder, cfg: FixConfig) -> (Stream<Burst<FixEvent>>, FixSender) {
    let (inject_sender, inject_receiver) = kanal::bounded::<FixMessage>(INJECT_QUEUE_CAPACITY);
    let fix_sender = FixSender {
        sender: inject_sender,
    };
    // The receiver is moved into the session thread at start; the backing channel
    // source is single-run, so `setup` fires exactly once.
    let inject_rx = Rc::new(RefCell::new(Some(inject_receiver)));

    let events = g.source_at_start::<FixEvent, _>(move |sender| {
        let inject_rx = inject_rx
            .borrow_mut()
            .take()
            .ok_or_else(|| anyhow::anyhow!("fix threaded source: already started (single-run)"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let cfg = cfg.clone();
        let handle = std::thread::Builder::new()
            .name("fix-session".into())
            .spawn(move || run_session_thread(cfg, inject_rx, sender, thread_stop))
            .context("fix: spawning session thread")?;
        Ok(StopHandle::new(ThreadedStopGuard {
            stop,
            thread: Some(handle),
        }))
    });

    (events, fix_sender)
}

/// Accept one initiator connection, checking `stop` between non-blocking accepts
/// so teardown ends the acceptor promptly instead of blocking forever.
fn accept_with_stop(port: u16, stop: &AtomicBool) -> io::Result<TcpStream> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    listener.set_nonblocking(true)?;
    loop {
        match listener.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false)?;
                return Ok(s);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Relaxed) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "stop requested"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Background session thread: connect (initiator) or bind+accept (acceptor), run
/// the session, and — for an established session that drops — reconnect
/// (initiators after a pause; acceptors immediately). Ends the loop (and closes
/// the channel) when the stop flag is set or the receiver is gone.
fn run_session_thread(
    cfg: FixConfig,
    inject_rx: kanal::Receiver<FixMessage>,
    chan: ChannelSender<FixEvent>,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let sock_result = if cfg.is_acceptor {
            accept_with_stop(cfg.port, &stop)
        } else {
            connect_with_retry(&cfg.host, cfg.port).map_err(|e| io::Error::other(e.to_string()))
        };

        let sock = match sock_result {
            Ok(s) => s,
            Err(e) => {
                if !chan.send(FixEvent::Status(FixSessionStatus::Error(e.to_string()))) {
                    break;
                }
                // Initiators give up on a connect failure; acceptors retry.
                if !cfg.is_acceptor {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        let mut session =
            FixSession::new_with_logon(&cfg.sender_comp_id, &cfg.target_comp_id, cfg.logon.clone());

        // A short read timeout lets the session loop flush the inject queue and
        // check the stop flag even when no data arrives (e.g. between heartbeats).
        let _ = sock.set_read_timeout(Some(THREADED_READ_TIMEOUT));

        let still_open = if cfg.tls {
            match tls_connect(&cfg.host, sock) {
                Ok(tls_stream) => run_fix_session(
                    tls_stream,
                    &mut session,
                    cfg.is_acceptor,
                    &inject_rx,
                    &chan,
                    &stop,
                ),
                Err(e) => {
                    if !chan.send(FixEvent::Status(FixSessionStatus::Error(e.to_string()))) {
                        break;
                    }
                    if !cfg.is_acceptor {
                        break;
                    }
                    continue;
                }
            }
        } else {
            run_fix_session(
                sock,
                &mut session,
                cfg.is_acceptor,
                &inject_rx,
                &chan,
                &stop,
            )
        };

        if !still_open {
            break; // channel closed or stop requested — exit the thread
        }

        if !chan.send(FixEvent::Status(FixSessionStatus::Disconnected)) {
            break;
        }

        // Reconnect after an established session dropped: acceptors loop to
        // re-accept immediately; initiators pause first so a flapping venue isn't
        // hammered. The fresh `FixSession` on the next iteration re-logs-in.
        if !cfg.is_acceptor {
            std::thread::sleep(RECONNECT_DELAY);
        }
    }

    // Best-effort end-of-stream so the receiver winds down.
    chan.close();
}

/// Run a single FIX session on `sock`, forwarding events to `chan`.
/// Outbound messages queued on `inject_rx` are flushed each loop iteration.
///
/// Returns `true` if the session ended due to a normal network disconnect (the
/// caller may reconnect), or `false` if the graph is gone / stop was requested
/// (the thread should exit).
fn run_fix_session<S: Read + Write>(
    sock: S,
    session: &mut FixSession,
    is_acceptor: bool,
    inject_rx: &kanal::Receiver<FixMessage>,
    chan: &ChannelSender<FixEvent>,
    stop: &AtomicBool,
) -> bool {
    if !chan.send(FixEvent::Status(FixSessionStatus::LoggingIn)) {
        return false;
    }

    let mut sock_opt = Some(sock);

    if !is_acceptor
        && let Some(s) = sock_opt.as_mut()
        && let Err(e) = session.send_logon(s)
    {
        let _ = chan.send(FixEvent::Status(FixSessionStatus::Error(e.to_string())));
        return true;
    }

    let mut parse_buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; READ_BUF_SIZE];

    loop {
        // Teardown → exit the thread (treated like a closed channel by the caller).
        if stop.load(Ordering::Relaxed) {
            return false;
        }

        let sock_ref = match sock_opt.as_mut() {
            Some(s) => s,
            None => return true, // disconnected during session dispatch
        };

        let got_data = match sock_ref.read(&mut tmp) {
            Ok(0) => return true,
            Err(e)
                if e.kind() == io::ErrorKind::ConnectionReset
                    || e.kind() == io::ErrorKind::BrokenPipe =>
            {
                return true;
            }
            // Read timeout or would-block: no data yet, but still flush the inject queue below.
            Err(e)
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock =>
            {
                false
            }
            Err(_) => return true, // shutdown or other error — clean exit
            Ok(n) => {
                parse_buf.extend_from_slice(&tmp[..n]);
                true
            }
        };

        let mut events: Burst<FixEvent> = TinyVec::new();
        if got_data
            && drain_parse_buf(
                &mut parse_buf,
                &mut sock_opt,
                session,
                &mut events,
                is_acceptor,
            )
            .is_err()
        {
            return true;
        }

        // Flush any outbound messages injected from outside the graph.
        // `try_recv` is lock-free; no lock is held across `session.send()`.
        if let Some(ref mut s) = sock_opt {
            loop {
                match inject_rx.try_recv() {
                    Ok(Some(msg)) => {
                        if session.send(s, &msg.msg_type, &msg.fields).is_err() {
                            return true;
                        }
                    }
                    Ok(None) => break, // queue empty
                    Err(_) => break,   // all senders dropped — nothing more will arrive
                }
            }
        }

        for event in events {
            if !chan.send(event) {
                return false;
            }
        }
    }
}

// ── Public factory functions ──────────────────────────────────────────────────

/// Connect to a FIX acceptor as an initiator (plain TCP).
///
/// Returns `(data_stream, status_stream)`. Realtime-only.
///
/// # Errors
///
/// Returns an error at wiring time if `run_mode` is [`RunMode::HistoricalFrom`]
/// (a live session has no historical timeline to replay).
pub fn fix_connect(
    g: &GraphBuilder,
    run_mode: RunMode,
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
) -> Result<(Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>)> {
    reject_historical(run_mode)?;
    let cfg = FixConfig {
        host: host.to_string(),
        port,
        sender_comp_id: sender_comp_id.to_string(),
        target_comp_id: target_comp_id.to_string(),
        logon: FixLogon::None,
        tls: false,
        is_acceptor: false,
    };
    let events = match mode {
        FixPollMode::AlwaysSpin => spin_source(g, cfg),
        FixPollMode::Threaded => threaded_source(g, cfg).0,
    };
    Ok(split_events(events))
}

/// Bind a FIX acceptor on `port`, accepting one initiator connection.
///
/// Returns `(data_stream, status_stream)`. Realtime-only.
///
/// # Errors
///
/// Returns an error at wiring time if `run_mode` is [`RunMode::HistoricalFrom`].
pub fn fix_accept(
    g: &GraphBuilder,
    run_mode: RunMode,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
) -> Result<(Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>)> {
    reject_historical(run_mode)?;
    let cfg = FixConfig {
        host: "0.0.0.0".to_string(),
        port,
        sender_comp_id: sender_comp_id.to_string(),
        target_comp_id: target_comp_id.to_string(),
        logon: FixLogon::None,
        tls: false,
        is_acceptor: true,
    };
    let events = match mode {
        FixPollMode::AlwaysSpin => spin_source(g, cfg),
        FixPollMode::Threaded => threaded_source(g, cfg).0,
    };
    Ok(split_events(events))
}

/// Connect to a TLS-secured FIX acceptor as an initiator.
///
/// Suitable for production-grade FIX gateways such as **LMAX London Demo**.
/// `sender_comp_id` should be your registered username; `password` is sent as
/// tag 554 in the Logon message (with tag 553 = `sender_comp_id`). Always uses
/// the `Threaded` poll mode. Realtime-only.
///
/// Returns a [`FixConnection`] with `data` and `status` streams, plus
/// [`fix_sub`](FixConnection::fix_sub) and [`send`](FixConnection::send).
///
/// # Errors
///
/// Returns an error at wiring time if `run_mode` is [`RunMode::HistoricalFrom`].
pub fn fix_connect_tls(
    g: &GraphBuilder,
    run_mode: RunMode,
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    password: Option<&str>,
) -> Result<FixConnection> {
    fix_connect_tls_logon(
        g,
        run_mode,
        host,
        port,
        sender_comp_id,
        target_comp_id,
        FixLogon::from(password),
    )
}

/// Connect to a TLS-secured FIX acceptor as an initiator with a custom Logon.
///
/// Same as [`fix_connect_tls`] but takes a [`FixLogon`] so the caller controls
/// the authentication fields (see [`FixLogon::custom`]). Realtime-only.
///
/// # Errors
///
/// Returns an error at wiring time if `run_mode` is [`RunMode::HistoricalFrom`].
pub fn fix_connect_tls_logon(
    g: &GraphBuilder,
    run_mode: RunMode,
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    logon: FixLogon,
) -> Result<FixConnection> {
    reject_historical(run_mode)?;
    let cfg = FixConfig {
        host: host.to_string(),
        port,
        sender_comp_id: sender_comp_id.to_string(),
        target_comp_id: target_comp_id.to_string(),
        logon,
        tls: true,
        is_acceptor: false,
    };
    let (events, sender) = threaded_source(g, cfg);
    let (data, status) = split_events(events);
    Ok(FixConnection {
        data,
        status,
        graph: g.clone(),
        sender,
    })
}

// ── FixOperators trait (fix_send sink) ─────────────────────────────────────────

/// Build a FIX MarketDataRequest (MsgType V) subscribing to top-of-book for `symbol`.
fn market_data_request(symbol: &str, req_id: &str) -> FixMessage {
    FixMessage {
        msg_type: "V".to_string(),
        seq_num: 0,
        sending_time: wingfoil_next::NanoTime::ZERO,
        fields: vec![
            (262, req_id.to_string()), // MDReqID
            (263, "1".to_string()),    // SubscriptionRequestType = Subscribe
            (264, "1".to_string()),    // MarketDepth = top of book
            (265, "0".to_string()),    // MDUpdateType = Full Refresh
            (267, "2".to_string()),    // NoMDEntryTypes = 2
            (269, "0".to_string()),    // MDEntryType = Bid
            (269, "1".to_string()),    // MDEntryType = Ask
            (146, "1".to_string()),    // NoRelatedSym = 1
            (48, symbol.to_string()),  // SecurityID
            (22, "8".to_string()),     // IDSource = Exchange Symbol
        ],
    }
}

/// Graph-thread-local state of a [`fix_send`](FixOperators::fix_send) sink:
/// its own outbound session, connected at graph `start()`.
struct FixSenderState {
    host: String,
    port: u16,
    session: FixSession,
    socket: Option<TcpStream>,
    parse_buf: Vec<u8>,
}

impl FixSenderState {
    /// Connect + logon (at graph `start()`). A connect failure aborts the run.
    fn connect(&mut self) -> anyhow::Result<()> {
        let mut sock = connect_with_retry(&self.host, self.port)
            .with_context(|| format!("fix_send: connecting to {}:{}", self.host, self.port))?;
        self.session.send_logon(&mut sock)?;
        sock.set_nonblocking(true)?;
        self.socket = Some(sock);
        Ok(())
    }

    /// Drain incoming session-level bytes (respond to heartbeats / test requests),
    /// then write `msg` on the established socket.
    fn write(&mut self, msg: &FixMessage) -> anyhow::Result<()> {
        let mut sock_opt = self.socket.take();

        // Drain any incoming bytes (heartbeats, test requests, etc.).
        if let Some(sock) = sock_opt.as_mut() {
            let mut tmp = [0u8; READ_BUF_SIZE];
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) => {
                        sock_opt = None;
                        break;
                    }
                    Ok(n) => self.parse_buf.extend_from_slice(&tmp[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        sock_opt = None;
                        break;
                    }
                }
            }
        }

        // Handle session-level messages (respond to test requests, etc.).
        let mut events: Burst<FixEvent> = TinyVec::new();
        drain_parse_buf(
            &mut self.parse_buf,
            &mut sock_opt,
            &mut self.session,
            &mut events,
            false,
        )?;

        let mut sock = sock_opt.ok_or_else(|| anyhow::anyhow!("FIX sender: connection lost"))?;
        self.session.send(&mut sock, &msg.msg_type, &msg.fields)?;
        self.socket = Some(sock);
        Ok(())
    }
}

/// Drops a [`fix_send`](FixOperators::fix_send) session at teardown with a
/// best-effort Logout (classic's `stop`).
struct FixSenderGuard(Rc<RefCell<FixSenderState>>);

impl Drop for FixSenderGuard {
    fn drop(&mut self) {
        let mut st = self.0.borrow_mut();
        let sock = st.socket.take();
        if let Some(mut sock) = sock {
            let _ = st.session.send_logout(&mut sock);
        }
    }
}

/// Fluent extension for sending a [`FixMessage`] stream to a FIX acceptor.
pub trait FixOperators {
    /// Open a dedicated outbound FIX session (connect + logon at graph `start()`)
    /// and send each [`FixMessage`] from the graph thread. Realtime-only: a
    /// historical run aborts at `start()` with a "real-time" error. Returns the
    /// sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// The returned [`Result`] is `Err` only if the *sink wiring* fails; a
    /// connection failure surfaces at graph `start()`, aborting the run.
    fn fix_send(
        &self,
        host: &str,
        port: u16,
        sender_comp_id: &str,
        target_comp_id: &str,
    ) -> Result<Stream<()>>;
}

impl FixOperators for Stream<FixMessage> {
    fn fix_send(
        &self,
        host: &str,
        port: u16,
        sender_comp_id: &str,
        target_comp_id: &str,
    ) -> Result<Stream<()>> {
        let state = Rc::new(RefCell::new(FixSenderState {
            host: host.to_string(),
            port,
            session: FixSession::new(sender_comp_id, target_comp_id),
            socket: None,
            parse_buf: Vec::new(),
        }));
        let start_state = state.clone();
        Ok(self.wire(move |b, h| {
            let sink = b.register_op1(
                h,
                "fix_send",
                Activation::NONE,
                state,
                || (),
                move |state: &mut Rc<RefCell<FixSenderState>>,
                      _s: &mut (),
                      msg: &FixMessage,
                      _ctx: &mut Ctx<'_>| {
                    state.borrow_mut().write(msg)?;
                    Ok(Tick::Value(()))
                },
            );
            // Connect + logon at graph start(); the run-mode check lives here too,
            // so a historical run aborts naming the run mode (classic ordering).
            // The returned guard's Drop sends a best-effort Logout at teardown.
            b.compose_spawn_at_start(sink.index(), move |run_mode, _run_for, _start_time| {
                if let RunMode::HistoricalFrom(_) = run_mode {
                    anyhow::bail!("FIX nodes only support real-time mode");
                }
                start_state.borrow_mut().connect()?;
                Ok(StopHandle::new(FixSenderGuard(start_state.clone())))
            });
            sink
        }))
    }
}

// ── Unit tests (pure codec / session; no network) ──────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let bytes = encode_message(
            "D",
            "SENDER",
            "TARGET",
            1,
            "20240627-11:17:25.223",
            &[
                (55, "AAPL".to_string()),
                (54, "1".to_string()),
                (38, "100".to_string()),
                (44, "150.00".to_string()),
            ],
        );
        let (msg_bytes, _) = find_message(&bytes).expect("message not found");
        let msg = build_message(decode_fields(&msg_bytes)).expect("parse failed");
        assert_eq!(msg.msg_type, "D");
        assert_eq!(msg.seq_num, 1);
        assert_eq!(msg.field(55), Some("AAPL"));
        assert_eq!(msg.field(54), Some("1"));
        assert_eq!(msg.field(38), Some("100"));
        assert_eq!(msg.field(44), Some("150.00"));
    }

    /// A [`FixLogon::Custom`] builder is invoked with the seq/SendingTime the
    /// Logon carries, and its returned fields land in the encoded message
    /// alongside the standard EncryptMethod/ResetSeqNumFlag defaults. This is
    /// the seam the Binance Ed25519 signer plugs into.
    #[test]
    fn custom_logon_fields_are_sent() {
        let mut session = FixSession::new_with_logon(
            "ME",
            "YOU",
            FixLogon::custom(|ctx: &LogonContext| {
                assert_eq!(ctx.msg_type, "A");
                assert_eq!(ctx.sender_comp_id, "ME");
                vec![
                    (553, "api-key".to_string()),
                    (96, format!("sig:{}:{}", ctx.msg_seq_num, ctx.sending_time)),
                ]
            }),
        );

        let mut buf: Vec<u8> = Vec::new();
        session.send_logon(&mut buf).expect("logon encodes");
        let (msg_bytes, _) = find_message(&buf).expect("message not found");
        let msg = build_message(decode_fields(&msg_bytes)).expect("parse failed");

        assert_eq!(msg.msg_type, "A");
        assert_eq!(msg.field(98), Some("0")); // EncryptMethod default
        assert_eq!(msg.field(141), Some("Y")); // ResetSeqNumFlag default
        assert_eq!(msg.field(553), Some("api-key"));
        // Signature was bound to the seq (1) and the wire SendingTime.
        assert!(
            msg.field(96).is_some_and(|s| s.starts_with("sig:1:")),
            "expected signed RawData bound to seq 1, got {:?}",
            msg.field(96)
        );
    }

    /// A [`FixLogon::Password`] session sends Username (tag 553 = SenderCompID)
    /// and Password (tag 554) in the Logon, alongside the standard defaults.
    #[test]
    fn password_logon_sends_username_and_password() {
        let mut session =
            FixSession::new_with_logon("ME", "YOU", FixLogon::Password("secret".to_string()));

        let mut buf: Vec<u8> = Vec::new();
        session.send_logon(&mut buf).expect("logon encodes");
        let (msg_bytes, _) = find_message(&buf).expect("message not found");
        let msg = build_message(decode_fields(&msg_bytes)).expect("parse failed");

        assert_eq!(msg.msg_type, "A");
        assert_eq!(msg.field(98), Some("0")); // EncryptMethod default
        assert_eq!(msg.field(141), Some("Y")); // ResetSeqNumFlag default
        assert_eq!(msg.field(553), Some("ME")); // Username = SenderCompID
        assert_eq!(msg.field(554), Some("secret")); // Password
    }

    /// Fills the inject channel to capacity with no draining receiver and
    /// asserts the next `send` returns `SendError::QueueFull`. Also confirms
    /// that `send_or_log` swallows the error (doesn't panic).
    #[test]
    fn fix_sender_queue_full() {
        let (tx, _rx) = kanal::bounded::<FixMessage>(INJECT_QUEUE_CAPACITY);
        let sender = FixSender { sender: tx };
        let msg = FixMessage::default();

        for i in 0..INJECT_QUEUE_CAPACITY {
            sender
                .send(msg.clone())
                .unwrap_or_else(|e| panic!("send {i} of {INJECT_QUEUE_CAPACITY} failed: {e}"));
        }
        assert_eq!(sender.send(msg.clone()), Err(SendError::QueueFull));
        // send_or_log on a full queue must not panic.
        sender.send_or_log(msg);
    }

    /// With the receiver dropped, `send` must return `SendError::Closed`.
    #[test]
    fn fix_sender_closed() {
        let (tx, rx) = kanal::bounded::<FixMessage>(INJECT_QUEUE_CAPACITY);
        let sender = FixSender { sender: tx };
        drop(rx);
        assert_eq!(sender.send(FixMessage::default()), Err(SendError::Closed));
    }
}
