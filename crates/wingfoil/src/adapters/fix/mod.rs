//! fix adapter — the FIX (Financial Information eXchange) protocol: a
//! synchronous, poll-based session engine (initiator + acceptor, plain TCP or
//! TLS). It ports the legacy `wingfoil::adapters::fix` module onto the Op
//! model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`statistics`](crate::adapters::statistics)
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
//!   enabled with `use wingfoil::adapters::fix::FixOperators;`: opens a
//!   dedicated outbound session and sends each message
//!   ([`fix_send`](FixOperators::fix_send)).
//!
//! # Poll modes (source machinery)
//!
//! FIX sessions are synchronous poll-based TCP connections, so — like the
//! legacy adapter and deliberately *unlike* the async adapters (etcd, redis)
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
//! reconnect. This matches legacy exactly.
//!
//! # Session management
//!
//! The session layer handles the parts of FIX 4.4 that make a feed
//! *trustworthy* rather than merely connected. All of it runs before a message
//! reaches your graph.
//!
//! **Sequence numbers.** Every inbound message's `MsgSeqNum` (tag 34) is checked
//! against the expected next value:
//!
//! - *In sequence* — dispatched, expectation advances.
//! - *Too high* — messages have been missed. A `ResendRequest` (35=2, from the
//!   expected number, `EndSeqNo=0`) is sent **once** per gap, a
//!   [`FixSessionStatus::SequenceGap`] is raised on the status stream, and the
//!   message is dropped so the counterparty's replay delivers it in order. The
//!   status event matters for order flow: between `expected` and `received` there
//!   may be fills you have not seen.
//! - *Too low with `PossDupFlag=Y`* — a legitimate resend of something already
//!   processed; ignored without advancing.
//! - *Too low without `PossDupFlag`* — unrecoverable per FIX 4.4. A `Logout`
//!   naming the reason is sent and the session terminates with an
//!   [`FixSessionStatus::Error`].
//!
//! `SequenceReset` (35=4) is honoured in both GapFill and Reset modes and is
//! processed **regardless of its own sequence number** — its purpose is to repair
//! a sequence that is already wrong. A `NewSeqNo` that would move the expectation
//! *backwards* is Rejected instead of applied.
//!
//! **Heartbeats.** The negotiated `HeartBtInt` is honoured in both directions: a
//! `Heartbeat` goes out once the session has been quiet for the interval, a
//! `TestRequest` probes a counterparty that has been quiet for 1.2× it, and a
//! counterparty still silent at 2.4× with an unanswered probe is declared gone
//! (`Threaded` then reconnects). An inbound `TestRequest` is answered with a
//! `Heartbeat` echoing its `TestReqID`. An acceptor adopts the initiator's
//! requested interval; `HeartBtInt=0` disables heartbeating entirely.
//!
//! **Persistence.** [`FixSeqNumStore`] decides whether sequence numbers survive a
//! reconnect. The default [`FixSeqNumStore::Reset`] sends `ResetSeqNumFlag=Y` on
//! every Logon — simple, but it cannot recover messages missed while
//! disconnected, and many venues refuse unsolicited intraday resets. Pass
//! [`FixSeqNumStore::File`] through [`FixOptions`] (see
//! [`fix_connect_tls_logon_with_options`]) for a session that resumes.
//!
//! **Rejects.** A frame that framed and checksummed cleanly but is unusable — no
//! `MsgType`, a `SequenceReset` whose `NewSeqNo` runs backwards — is answered
//! with a session-level `Reject` (35=3) carrying a `SessionRejectReason` (tag
//! 373). A **garbled** frame is not: one that fails BodyLength or CheckSum is
//! logged and dropped, because a Reject names its target by `MsgSeqNum` and that
//! is precisely the field a failed integrity check leaves untrustworthy. See
//! [`find_message`].
//!
//! An inbound `Reject` is *delivered* to your graph rather than consumed — it is
//! how a venue tells you an order was malformed.
//!
//! **Resend.** [`FixMessageStore`] decides what an inbound `ResendRequest` gets
//! back. The default [`FixMessageStore::None`] answers the whole range with
//! `SequenceReset`-`GapFill` — conformant, and right for a session with nothing
//! to resend, but it means the counterparty does not get your orders back.
//! [`FixMessageStore::Memory`] retains the last N application messages and
//! replays the requested ones at their original `MsgSeqNum` with `PossDupFlag=Y`
//! and `OrigSendingTime`, gap-filling only the admin messages and the ranges
//! that have aged out. Order-flow sessions generally want it; market-data
//! subscriptions do not need it.
//!
//! ## What this is still not
//!
//! **The message store is in memory, so it does not survive a process
//! restart.** It covers the case that actually happens — a session drops and
//! reconnects within the same process — but after a restart a `ResendRequest`
//! degrades to gap fill for everything. Pair it with [`FixSeqNumStore::File`] so
//! the sequence numbers at least resume; a durable message store (framing,
//! rotation, fsync policy) is a larger piece of work and is deliberately not
//! pretended at here. In that window the venue's drop-copy feed or an
//! OrderStatusRequest is still the recovery path.
//!
//! This is not a certified FIX engine and has not been through a venue
//! conformance suite. If you need certification, or FIX 5.x / FIXT, or a
//! durable outbound store, drive a dedicated engine and bridge it into the
//! graph over [`iceoryx2`](crate::adapters::iceoryx2) or
//! [`aeron`](crate::adapters::aeron).
//!
//! # Codec
//!
//! Messages are framed on **BodyLength** (tag 9) and their **CheckSum** (tag 10)
//! is verified before anything is dispatched; a frame that fails either is
//! dropped and the buffer resynchronises to the next `8=`. Length-delimited data
//! fields (95/96 `RawData`, 212/213 `XmlData`, the `Encoded*` pairs) are decoded
//! using their length field, so a payload may contain arbitrary bytes — SOH
//! included — as the spec allows.
//!
//! Repeating groups are addressable: [`FixMessage::groups`] splits a group into
//! per-entry [`FixGroup`] views and [`FixMessage::fields_all`] returns every value
//! for a repeated tag. [`FixMessage::field`] returns only the *first* match, which
//! is wrong for anything inside a group — a two-sided
//! MarketDataSnapshot has two `MDEntryPx` (270) values and `field(270)` sees only
//! the bid. `SendingTime` (tag 52) is parsed into
//! [`FixMessage::sending_time`].
//!
//! # Sink
//!
//! [`FixOperators::fix_send`] opens its own outbound session (connect + logon at
//! graph `start()`, realtime-only) and writes each [`FixMessage`] from the graph
//! thread. A send is **not** guaranteed to reach the wire before it returns:
//! when the non-blocking socket fills mid-frame, the unwritten suffix is
//! retained on the session and goes out ahead of whatever is written next, so
//! the counterparty always sees whole frames in sequence order. A historical run aborts
//! at `start()` with a "real-time" error (matching legacy's `start` check). The
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
//! `RawData`, tag 96, signed over tags 35/49/56/34/52 joined by SOH). Wingfoil
//! stays free of venue/crypto specifics — the signer lives in the caller.
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* is preserved — both poll modes, initiator and
//! acceptor, TLS, reconnect, the [`FixSender`] inject channel with its
//! [`SendError`] policy, [`fix_sub`](FixConnection::fix_sub), and
//! [`fix_send`](FixOperators::fix_send). The surface differs in the deliberate,
//! systemic ways every wingfoil adapter does:
//!
//! 1. **The source factories take a [`GraphBuilder`] and a [`RunMode`].** Every
//!    wingfoil source wires onto a builder, and a live source needs the run mode to
//!    reject `HistoricalFrom` at wiring (legacy checked real-time-ness at run
//!    `start()`; wingfoil rejects earlier, at wiring). The message is the same
//!    ("real-time").
//! 2. **The sources return [`Stream`]s, not `Rc<dyn Stream>`;
//!    [`fix_send`](FixOperators::fix_send) returns `Result<Stream<()>>` and
//!    [`fix_sub`](FixConnection::fix_sub) a `Stream<()>`** (not `Rc<dyn Node>`) —
//!    the wingfoil stream/sink types. The socket connect + logon still happen at
//!    graph `start()` (like legacy's `start`), the Logout at teardown (like
//!    `stop`).
//! 3. **No `AlwaysSpin` socket-shutdown fast-path in `Threaded` teardown.** The
//!    background session loop checks a stop flag against its 200 ms read timeout
//!    (the [`zmq`](crate::adapters::zmq) pattern) instead of legacy's
//!    `Arc<Mutex<Option<TcpStream>>>` shutdown handle, so there is no lock on the
//!    graph path; teardown costs up to one read-timeout (200 ms) longer.
//!
//! The single-value convenience sink other adapters offer is not applicable
//! (the sink element is `FixMessage`, not a `Burst`).
//!
//! ## Where wingfoil is a superset
//!
//! The session and codec work above is **new capability** — legacy's adapter has
//! none of it, so none of this is a parity gap in either direction, but it does
//! mean the two trees no longer behave identically on a malformed or
//! out-of-sequence feed. Legacy accepts both; wingfoil does not:
//!
//! 4. **Sequence validation, resend, and Reject generation.** Legacy parses tag 34
//!    and never compares it, so a gap passes through silently and a lost
//!    ExecutionReport is undetectable. Legacy also never validates an inbound
//!    CheckSum or uses BodyLength for framing (it scans for `\x0110=`, which
//!    mis-frames any length-delimited payload), and never sends a Reject. Both
//!    trees *drop* a garbled frame rather than answering it — wingfoil because
//!    the spec requires it, legacy because it cannot tell.
//! 5. **An outbound heartbeat timer.** Legacy advertises `HeartBtInt` and only
//!    ever sends a `Heartbeat` in reply to a `TestRequest`, so survival depends on
//!    the venue probing before it disconnects.
//! 6. **Sequence-number persistence.** Legacy is in-memory only and always sends
//!    `ResetSeqNumFlag=Y`. That remains wingfoil's *default*
//!    ([`FixSeqNumStore::Reset`]) so the out-of-the-box conversation with a venue
//!    is unchanged; [`FixSeqNumStore::File`] is opt-in.
//! 7. **`SendingTime` is parsed** into [`FixMessage::sending_time`] rather than
//!    left at [`NanoTime::ZERO`](wingfoil::NanoTime::ZERO), and repeating groups
//!    are addressable ([`FixMessage::groups`]).
//! 8. **Whole frames under backpressure.** Both trees write with `write_all` on a
//!    non-blocking socket in spin mode, which returns on `WouldBlock` having
//!    written a prefix — so legacy can put a torn frame on the wire and then
//!    write the next message on top of it. wingfoil retains the unwritten suffix
//!    and sends it ahead of anything else, which makes a send *deferred* rather
//!    than synchronous: `Ok` means queued in order, not on the wire.
//!
//! See also [`deviation-register.md`](../../../../docs/planning/deviation-register.md).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use tinyvec::TinyVec;
use wingfoil::RunMode;

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
const TAG_POSS_DUP_FLAG: u32 = 43;
const TAG_BEGIN_SEQ_NO: u32 = 7;
const TAG_ORIG_SENDING_TIME: u32 = 122;
const TAG_END_SEQ_NO: u32 = 16;
const TAG_NEW_SEQ_NO: u32 = 36;
const TAG_GAP_FILL_FLAG: u32 = 123;
const TAG_REF_SEQ_NUM: u32 = 45;
const TAG_SESSION_REJECT_REASON: u32 = 373;

const MSG_HEARTBEAT: &str = "0";
const MSG_TEST_REQUEST: &str = "1";
const MSG_RESEND_REQUEST: &str = "2";
const MSG_REJECT: &str = "3";
const MSG_SEQUENCE_RESET: &str = "4";
const MSG_LOGOUT: &str = "5";
const MSG_LOGON: &str = "A";

/// `SessionRejectReason` (tag 373) "Value is incorrect (out of range) for this
/// tag" — used for a NewSeqNo that would move the expected inbound sequence
/// backwards.
///
/// A *framing* failure gets no Reject at all: see [`find_message`] for why a
/// garbled frame is ignored rather than answered.
const REJECT_VALUE_OUT_OF_RANGE: u32 = 5;

/// `SessionRejectReason` (tag 373) "Required tag missing" — a frame that
/// checksummed cleanly but carries no `MsgType` (35).
const REJECT_REQUIRED_TAG_MISSING: u32 = 1;

const SOH: u8 = 0x01;
const BEGIN_STRING: &str = "FIX.4.4";
const READ_BUF_SIZE: usize = 4096;

/// Default `HeartBtInt` (tag 108) offered in a Logon, in seconds. An acceptor
/// adopts whatever the initiator asks for instead (see
/// [`FixSession::adopt_heartbeat_interval`]).
const HEARTBEAT_INTERVAL: u32 = 30;

/// Sanity ceiling on `BodyLength` (tag 9) before the frame is rejected outright,
/// so a corrupt length cannot make the session buffer without bound waiting for
/// a message that will never arrive. Comfortably above any real FIX message.
const MAX_BODY_LENGTH: usize = 1 << 20;

/// Multiple of the heartbeat interval with no inbound traffic after which the
/// session sends a `TestRequest`, per FIX's "some reasonable transmission time"
/// guidance (20% is the conventional grace).
const TEST_REQUEST_AFTER: f64 = 1.2;

/// Multiple of the heartbeat interval with no inbound traffic — and an
/// unanswered `TestRequest` — after which the counterparty is declared
/// unresponsive and the session is dropped.
const DISCONNECT_AFTER: f64 = 2.4;

/// Pause before an initiator re-connects after an established session dropped, so
/// a flapping venue isn't hammered. (Connect *failures* still give up — this
/// covers only a session that logged in and later disconnected.)
pub const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Read timeout on the `Threaded` session socket: bounds how long the session
/// loop blocks in `read()` before checking the stop flag (and flushing the
/// inject queue) — the deferred-teardown budget in place of legacy's socket
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
    /// SendingTime (tag 52), parsed to [`NanoTime`](wingfoil::NanoTime).
    ///
    /// [`NanoTime::ZERO`](wingfoil::NanoTime::ZERO) if the field was absent or
    /// unparseable — which is also what an *outbound* message carries, since the
    /// header is stamped at send time.
    pub sending_time: wingfoil::NanoTime,
    /// Application-level tag/value pairs, in wire order (standard header and
    /// trailer excluded).
    ///
    /// Order is preserved because repeating groups are positional: see
    /// [`groups`](Self::groups).
    pub fields: Vec<(u32, String)>,
}

impl FixMessage {
    /// The **first** value for `tag`, if present in the application fields.
    ///
    /// Correct for non-repeating fields. For a tag that appears inside a
    /// repeating group this returns only the first entry's value, which is
    /// almost never what you want — use [`groups`](Self::groups) or
    /// [`fields_all`](Self::fields_all) instead.
    pub fn field(&self, tag: u32) -> Option<&str> {
        self.fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_str())
    }

    /// Every value for `tag`, in wire order.
    ///
    /// The flat way to read a repeating group when you only need one tag out of
    /// it — e.g. every `MDEntryPx` (270) in a MarketDataSnapshot. Use
    /// [`groups`](Self::groups) when entries must stay correlated across tags.
    pub fn fields_all(&self, tag: u32) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(move |(t, _)| *t == tag)
            .map(|(_, v)| v.as_str())
    }

    /// The declared entry count of a repeating group, from its `NoXxx` tag.
    ///
    /// `None` if the tag is absent or does not parse as a number.
    pub fn group_count(&self, count_tag: u32) -> Option<usize> {
        self.field(count_tag).and_then(|v| v.parse().ok())
    }

    /// Split a repeating group into its entries.
    ///
    /// `count_tag` is the group's `NoXxx` field (e.g. 268 `NoMDEntries`) and
    /// `delimiter_tag` is the tag that must begin each entry (e.g. 269
    /// `MDEntryType`). Returns one [`FixGroup`] per entry found, capped at the
    /// declared count; an empty `Vec` if the count tag is absent, zero, or no
    /// delimiter follows it.
    ///
    /// ```ignore
    /// for entry in msg.groups(268, 269) {
    ///     let side = entry.field(269);          // 0 = bid, 1 = offer
    ///     let px = entry.field(270);
    ///     let qty = entry.field(271);
    /// }
    /// ```
    ///
    /// # Limitation: the last entry's end
    ///
    /// Each entry runs from its delimiter up to the next one. Where the *last*
    /// entry ends cannot be determined without a data dictionary — nothing on
    /// the wire distinguishes "a field belonging to the final group entry" from
    /// "a field after the group" — so the last entry extends to the end of the
    /// message. Read named tags off it with [`FixGroup::field`] (which is scoped
    /// to the entry) rather than iterating it blindly, and it does not matter.
    ///
    /// Nested groups are not decomposed: an inner group's fields appear inline
    /// in the outer entry, and calling `groups` on the message again with the
    /// inner tags will not respect the outer boundaries. If you need that, walk
    /// [`fields`](Self::fields) yourself — it is in wire order.
    pub fn groups(&self, count_tag: u32, delimiter_tag: u32) -> Vec<FixGroup<'_>> {
        let Some(declared) = self.group_count(count_tag) else {
            return Vec::new();
        };
        if declared == 0 {
            return Vec::new();
        }
        // The group body begins after the count tag, so a delimiter-valued tag
        // appearing *before* it (in an unrelated part of the message) is skipped.
        let Some(count_at) = self.fields.iter().position(|(t, _)| *t == count_tag) else {
            return Vec::new();
        };
        let starts: Vec<usize> = self
            .fields
            .iter()
            .enumerate()
            .skip(count_at + 1)
            .filter(|(_, (t, _))| *t == delimiter_tag)
            .map(|(i, _)| i)
            .take(declared)
            .collect();
        starts
            .iter()
            .enumerate()
            .map(|(n, &start)| {
                let end = starts.get(n + 1).copied().unwrap_or(self.fields.len());
                FixGroup {
                    fields: &self.fields[start..end],
                }
            })
            .collect()
    }
}

/// One entry of a repeating group, borrowed from a [`FixMessage`].
///
/// See [`FixMessage::groups`] for how entry boundaries are determined, and for
/// the one case where they are approximate.
#[derive(Debug, Clone, Copy)]
pub struct FixGroup<'a> {
    fields: &'a [(u32, String)],
}

impl<'a> FixGroup<'a> {
    /// The first value for `tag` **within this entry**.
    pub fn field(&self, tag: u32) -> Option<&'a str> {
        self.fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_str())
    }

    /// This entry's tag/value pairs, in wire order.
    pub fn fields(&self) -> &'a [(u32, String)] {
        self.fields
    }
}

/// FIX session lifecycle state.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum FixSessionStatus {
    /// No transport: never connected, or the socket has dropped. An initiator
    /// in this state is between reconnect attempts.
    #[default]
    Disconnected,
    /// Connected, `Logon` (MsgType A) sent, awaiting the counterparty's.
    LoggingIn,
    /// Logon complete — the session is up and application messages flow.
    LoggedIn,
    /// Server sent a Logout (MsgType 5). Contains the `Text` field (tag 58) if present.
    LoggedOut(Option<String>),
    /// An inbound message arrived past the expected `MsgSeqNum`, so messages
    /// have been missed. A `ResendRequest` has been sent and the counterparty's
    /// replay is expected to close the gap; the message that revealed it was
    /// dropped and will arrive again in order.
    ///
    /// Surfaced as a status event rather than handled silently because for order
    /// flow a gap is a business event: between `expected` and `received` there
    /// may be fills you have not seen.
    SequenceGap {
        /// The `MsgSeqNum` (34) the session was expecting next.
        expected: u64,
        /// The `MsgSeqNum` that actually arrived. Everything in
        /// `expected..received` was missed.
        received: u64,
    },
    /// A transport or protocol failure, carrying its message — a refused
    /// connect, a malformed frame, a rejected logon. Reported as a status
    /// event rather than failing the run, so the graph can react to a session
    /// problem the way it reacts to any other value.
    Error(String),
}

/// Internal event multiplexing data messages and session status changes over the
/// one transport. Split back into the `(data, status)` streams before it reaches
/// the caller, so a status transition stays correctly ordered relative to the
/// messages around it.
#[derive(Debug, Clone)]
pub enum FixEvent {
    /// An inbound application message.
    Data(FixMessage),
    /// A session lifecycle transition. Interleaved with `Data` on the one
    /// transport precisely so its ordering against surrounding messages is
    /// preserved.
    Status(FixSessionStatus),
}

impl Default for FixEvent {
    fn default() -> Self {
        FixEvent::Data(FixMessage::default())
    }
}

/// Where a session's sequence numbers live between connections.
///
/// FIX sequence numbers are per-session state, not per-connection: after a
/// reconnect (or a process restart) both sides are expected to carry on from
/// where they left off, which is what makes gap detection and resend meaningful
/// across a drop.
#[derive(Debug, Clone, Default)]
pub enum FixSeqNumStore {
    /// In-memory only. Every Logon carries `ResetSeqNumFlag=Y`, telling the
    /// counterparty to restart both directions at 1.
    ///
    /// The default, and the simplest thing that works against venues that permit
    /// it. Two consequences to be aware of: a reconnect **cannot** recover
    /// messages missed while disconnected (the sequence both sides would use to
    /// identify them is gone), and venues that rate-limit or refuse unsolicited
    /// resets — many do, especially intraday — will reject the Logon.
    #[default]
    Reset,
    /// Persist the sequence numbers to `path`, resuming from them on the next
    /// connection and sending `ResetSeqNumFlag=N`.
    ///
    /// The file holds two decimal numbers (outbound, next expected inbound) and
    /// is rewritten in place after every message. It is **not** `fsync`ed, so a
    /// clean process restart resumes exactly while a machine-level crash may lose
    /// the last few numbers — recoverable, since the counterparty will then see a
    /// gap and the resend logic handles it.
    ///
    /// One write syscall per message: negligible on the [`FixPollMode::Threaded`]
    /// session thread, but on [`FixPollMode::AlwaysSpin`] it lands on the **graph
    /// thread**, which is not what that mode is for. Pair persistence with
    /// `Threaded`.
    File(std::path::PathBuf),
}

/// Whether the session retains the application messages it has sent, so an
/// inbound `ResendRequest` can be answered with the real messages rather than a
/// `SequenceReset`-`GapFill`.
///
/// FIX 4.4 gives the counterparty the right to ask for a range back, and a
/// session with nothing stored can only answer "skip it". For market-data
/// subscriptions that is fine. For **order flow** it is not: the range the venue
/// is asking for is exactly the range containing your orders and cancels, and
/// some venues require a genuine replay rather than accepting a gap fill.
#[derive(Debug, Clone, Default)]
pub enum FixMessageStore {
    /// Retain nothing. An inbound `ResendRequest` is answered with a
    /// `SequenceReset`-`GapFill` over the whole requested range — conformant,
    /// and the right answer for a session that genuinely has nothing to resend.
    ///
    /// The default, so an existing session's conversation with a venue does not
    /// change under it.
    #[default]
    None,
    /// Retain the last `capacity` **application** messages in memory and replay
    /// the ones the counterparty asks for, gap-filling only the ranges that were
    /// admin messages or have aged out.
    ///
    /// Covers the case that actually happens: a session drops and reconnects
    /// within the same process, and the venue asks for what it missed. It does
    /// **not** survive a process restart — the store is in memory — so a restart
    /// still degrades to gap fill for anything the counterparty asks for. Pair
    /// with [`FixSeqNumStore::File`] so at least the sequence numbers resume;
    /// a durable message store is a larger piece of work (framing, rotation,
    /// fsync policy) and is deliberately not pretended at here.
    ///
    /// Capacity is a message count, not bytes. Size it against the reconnect
    /// window you care about: a session sending 100 orders/second that must
    /// survive a 30-second drop wants ~3,000.
    Memory {
        /// How many sent messages to retain, as a **count** — size it against
        /// the reconnect window you care about, per the note above. Older
        /// entries are dropped and degrade to gap fill if requested.
        capacity: usize,
    },
}

/// One sent application message, kept so it can be replayed on request.
///
/// Fields rather than the encoded frame: a resend has to carry the *original*
/// `MsgSeqNum` (34) and `SendingTime` (52) while adding `PossDupFlag` (43) and
/// `OrigSendingTime` (122), which changes `BodyLength` and `CheckSum`. Storing
/// the frame would mean re-parsing it to rebuild it, so store what the encoder
/// wants instead.
struct SentMessage {
    seq: u64,
    msg_type: String,
    sending_time: String,
    fields: Vec<(u32, String)>,
}

/// The retained-message ring behind [`FixMessageStore`].
///
/// Only application messages are retained. Admin messages (Logon, Logout,
/// Heartbeat, TestRequest, ResendRequest, Reject, SequenceReset) are never
/// resent under FIX 4.4 — they are gap-filled — so keeping them would only make
/// the ring evict the messages that matter sooner.
#[derive(Default)]
struct SentStore {
    /// Ascending by `seq` — pushed in send order, popped from the front when
    /// full, so binary-searchable and cheap to walk over a requested range.
    sent: VecDeque<SentMessage>,
    /// Zero for [`FixMessageStore::None`], which makes every `record` a no-op
    /// and every lookup empty.
    capacity: usize,
}

impl SentStore {
    fn new(store: &FixMessageStore) -> Self {
        Self {
            sent: VecDeque::new(),
            capacity: match store {
                FixMessageStore::None => 0,
                FixMessageStore::Memory { capacity } => *capacity,
            },
        }
    }

    fn enabled(&self) -> bool {
        self.capacity > 0
    }

    /// Retain one sent application message, evicting the oldest if full.
    fn record(&mut self, seq: u64, msg_type: &str, sending_time: &str, fields: &[(u32, String)]) {
        if !self.enabled() || is_admin_msg_type(msg_type) {
            return;
        }
        if self.sent.len() == self.capacity {
            self.sent.pop_front();
        }
        self.sent.push_back(SentMessage {
            seq,
            msg_type: msg_type.to_string(),
            sending_time: sending_time.to_string(),
            fields: fields.to_vec(),
        });
    }

    /// Everything retained in `[begin, end]`, ascending.
    fn range(&self, begin: u64, end: u64) -> impl Iterator<Item = &SentMessage> {
        self.sent
            .iter()
            .filter(move |m| m.seq >= begin && m.seq <= end)
    }

    /// Dropped on a sequence reset: the numbers those messages were filed under
    /// no longer mean anything.
    fn clear(&mut self) {
        self.sent.clear();
    }
}

/// Whether `msg_type` is a session-level (admin) message. Admin messages are
/// gap-filled on resend rather than replayed — resending a stale Heartbeat or
/// Logon is meaningless and, for `SequenceReset`, actively harmful.
fn is_admin_msg_type(msg_type: &str) -> bool {
    matches!(
        msg_type,
        MSG_HEARTBEAT
            | MSG_TEST_REQUEST
            | MSG_RESEND_REQUEST
            | MSG_REJECT
            | MSG_SEQUENCE_RESET
            | MSG_LOGOUT
            | MSG_LOGON
    )
}

/// The open handle behind [`FixSeqNumStore`], plus whether a Logon should ask
/// for a reset.
struct SeqNumFile {
    file: Option<std::fs::File>,
    /// True for [`FixSeqNumStore::Reset`] — send `ResetSeqNumFlag=Y`.
    resets_on_logon: bool,
    /// Set once if a write failed, so a broken path warns rather than spamming.
    warned: bool,
}

impl SeqNumFile {
    fn open(store: &FixSeqNumStore) -> Self {
        match store {
            FixSeqNumStore::Reset => Self {
                file: None,
                resets_on_logon: true,
                warned: false,
            },
            FixSeqNumStore::File(path) => {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(path);
                match file {
                    Ok(f) => Self {
                        file: Some(f),
                        resets_on_logon: false,
                        warned: false,
                    },
                    Err(e) => {
                        // A store we cannot open must not silently degrade into
                        // "reset every logon" — that is a different protocol
                        // conversation with the venue. Warn loudly and keep the
                        // reset behaviour so the session still establishes.
                        log::warn!(
                            "fix: cannot open sequence-number store {}: {e}; \
                             falling back to ResetSeqNumFlag=Y on logon",
                            path.display()
                        );
                        Self {
                            file: None,
                            resets_on_logon: true,
                            warned: true,
                        }
                    }
                }
            }
        }
    }

    /// `(outbound, next expected inbound)` — from the file if it holds a usable
    /// pair, else FIX's starting point.
    fn load(&mut self) -> (u64, u64) {
        let default = (0, 1);
        let Some(file) = self.file.as_mut() else {
            return default;
        };
        let mut text = String::new();
        if std::io::Seek::seek(file, io::SeekFrom::Start(0)).is_err()
            || file.read_to_string(&mut text).is_err()
        {
            return default;
        }
        let mut parts = text.split_whitespace();
        match (
            parts.next().and_then(|s| s.parse().ok()),
            parts.next().and_then(|s| s.parse().ok()),
        ) {
            (Some(out), Some(inbound)) => (out, inbound),
            _ => default,
        }
    }

    fn save(&mut self, out_seq: u64, in_seq: u64) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        // Fixed-width so an in-place rewrite never leaves a longer previous
        // value trailing behind the new one.
        let record = format!("{out_seq:020} {in_seq:020}\n");
        let wrote = std::io::Seek::seek(file, io::SeekFrom::Start(0))
            .and_then(|_| file.write_all(record.as_bytes()));
        if wrote.is_err() && !self.warned {
            self.warned = true;
            log::warn!(
                "fix: sequence-number store write failed; sequences will not survive a restart"
            );
        }
    }
}

/// Whether the counterparty still looks alive, per
/// [`FixSession::maintain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    Alive,
    /// No inbound traffic for [`DISCONNECT_AFTER`] × `HeartBtInt` and an
    /// unanswered `TestRequest` — drop the connection.
    Unresponsive,
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

/// FIX 4.4 length-delimited `data` fields, as `(data tag, its length tag)`.
///
/// A data field's value may contain **any** byte, SOH included, so it is
/// delimited by the immediately preceding length field rather than by SOH. A
/// decoder that splits purely on SOH mis-parses every message carrying one —
/// which for this adapter includes its own `FixLogon::Custom` signature path
/// (tag 96), the reason the list is worth carrying.
const DATA_FIELDS: &[(u32, u32)] = &[
    (91, 90),   // SecureData / SecureDataLen
    (96, 95),   // RawData / RawDataLength
    (213, 212), // XmlData / XmlDataLen
    (349, 348), // EncodedIssuer
    (351, 350), // EncodedSecurityDesc
    (353, 352), // EncodedListExecInst
    (355, 354), // EncodedText
    (357, 356), // EncodedSubject
    (359, 358), // EncodedHeadline
    (361, 360), // EncodedAllocText
    (363, 362), // EncodedUnderlyingIssuer
    (365, 364), // EncodedUnderlyingSecurityDesc
    (446, 445), // EncodedListStatusText
    (619, 618), // EncodedLegIssuer
    (622, 621), // EncodedLegSecurityDesc
];

/// The length tag that must immediately precede `tag`, if `tag` is a data field.
fn data_length_tag(tag: u32) -> Option<u32> {
    DATA_FIELDS
        .iter()
        .find(|(data, _)| *data == tag)
        .map(|(_, len)| *len)
}

fn decode_fields(data: &[u8]) -> Vec<(u32, String)> {
    let mut fields: Vec<(u32, String)> = Vec::new();
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

        // A data field's length comes from the field before it, not from
        // scanning for SOH.
        let declared_len = data_length_tag(tag).and_then(|len_tag| {
            let (prev_tag, prev_value) = fields.last()?;
            (*prev_tag == len_tag).then(|| prev_value.parse::<usize>().ok())?
        });

        let (value_end, next) = match declared_len {
            Some(n) if eq + 1 + n <= data.len() => (eq + 1 + n, eq + 1 + n + 1),
            // No usable length (or it overruns the frame): fall back to SOH.
            _ => {
                let Some(soh_off) = data[eq + 1..].iter().position(|&b| b == SOH) else {
                    break;
                };
                let soh = eq + 1 + soh_off;
                (soh, soh + 1)
            }
        };
        // Lossy rather than empty: a binary data field should not silently
        // become "" just because it is not UTF-8.
        let value = String::from_utf8_lossy(&data[eq + 1..value_end]).into_owned();
        fields.push((tag, value));
        pos = next;
    }
    fields
}

/// Parse a FIX `SendingTime` / `OrigSendingTime` (`YYYYMMDD-HH:MM:SS` with an
/// optional fractional part) into a [`NanoTime`](wingfoil::NanoTime).
///
/// FIX 4.4 permits second, millisecond and microsecond precision, and venues in
/// practice also send nanoseconds; `%.f` accepts any of them. Returns `None` for
/// an absent or unparseable value, which the caller maps to `NanoTime::ZERO`
/// rather than rejecting the message — a bad timestamp on an otherwise valid
/// ExecutionReport should not cost you the fill.
fn parse_sending_time(value: &str) -> Option<wingfoil::NanoTime> {
    let dt = chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d-%H:%M:%S%.f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d-%H:%M:%S"))
        .ok()?;
    Some(wingfoil::NanoTime::from(dt))
}

fn build_message(all: Vec<(u32, String)>) -> Option<FixMessage> {
    let msg_type = all.iter().find(|(t, _)| *t == TAG_MSG_TYPE)?.1.clone();
    let seq_num = all
        .iter()
        .find(|(t, _)| *t == TAG_MSG_SEQ_NUM)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let sending_time = all
        .iter()
        .find(|(t, _)| *t == TAG_SENDING_TIME)
        .and_then(|(_, v)| parse_sending_time(v))
        .unwrap_or(wingfoil::NanoTime::ZERO);
    let fields = all
        .into_iter()
        .filter(|(t, _)| !HEADER_TAGS.contains(t))
        .collect();
    Some(FixMessage {
        msg_type,
        seq_num,
        sending_time,
        fields,
    })
}

/// Why a frame at the head of the read buffer could not be accepted.
///
/// Every variant is a **garbled** frame in the spec's sense, so none of them
/// produces a Reject — see [`find_message`]. The variant exists to say what went
/// wrong in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameError {
    /// No `8=FIX...` at the head of the buffer, or a `9=` that is not a number.
    Header,
    /// `BodyLength` (tag 9) did not land on the `10=` trailer.
    BodyLength,
    /// `CheckSum` (tag 10) did not match the computed sum.
    CheckSum,
}

impl FrameError {
    fn text(self) -> &'static str {
        match self {
            FrameError::Header => "malformed message header",
            FrameError::BodyLength => "BodyLength does not match the frame",
            FrameError::CheckSum => "CheckSum mismatch",
        }
    }
}

/// Outcome of framing the head of the read buffer.
#[derive(Debug)]
enum Frame {
    /// A complete, checksum-validated message. `consumed` bytes may be dropped
    /// from the buffer.
    Message { bytes: Vec<u8>, consumed: usize },
    /// Not enough bytes yet — leave the buffer alone and read more.
    Incomplete,
    /// A malformed frame. `consumed` bytes must be dropped to resynchronise (up
    /// to and including the bad frame, or past the junk before the next `8=`).
    Malformed { error: FrameError, consumed: usize },
}

/// Locate the next `8=` that could begin a message, at or after `from`.
fn next_begin_string(buf: &[u8], from: usize) -> Option<usize> {
    if from >= buf.len() {
        return None;
    }
    buf[from..]
        .windows(2)
        .position(|w| w == b"8=")
        .map(|p| from + p)
}

/// Frame the message at the head of `buf` using **BodyLength** (tag 9) and
/// verify **CheckSum** (tag 10).
///
/// The previous implementation scanned for the first `\x0110=` and treated
/// whatever preceded it as one message, ignoring tag 9 and never checking tag
/// 10. That accepts a corrupt message silently, and mis-frames any message whose
/// *payload* contains those bytes — which length-delimited fields (95/96
/// `RawData`, 212/213 `XmlData`) are explicitly allowed to do, SOH included.
/// Framing on the declared length is both the spec's rule and the only way to
/// carry binary payloads at all.
///
/// # A garbled frame is ignored, not Rejected
///
/// [`Frame::Malformed`] is dropped with a log line and no reply. FIX 4.4 is
/// explicit that a message failing BodyLength or CheckSum validation is
/// *garbled* and must be ignored rather than Rejected: a Reject names the
/// offending message by `RefSeqNum`, and the `MsgSeqNum` of a frame that failed
/// its own integrity check is exactly the field that cannot be trusted. Naming
/// it would tell the counterparty to act on a number we made up.
///
/// It is also the difference between absorbing corruption and amplifying it.
/// Every Reject consumes an outbound sequence number (and, with
/// [`FixSeqNumStore::File`], a write), so answering a corrupt stream frame for
/// frame burns the session's sequence space at the rate the peer sends junk.
///
/// A frame that *does* checksum cleanly is trustworthy enough to Reject, and
/// still is — see the missing-`MsgType` path in [`drain_parse_buf`].
fn find_message(buf: &[u8]) -> Frame {
    // Resynchronise: drop anything before the next plausible `8=`.
    let Some(start) = next_begin_string(buf, 0) else {
        // No `8=` anywhere, so nothing held here can begin a message. Drop all
        // of it but the last byte, which may be a bare `8` whose `=` is in the
        // next read. Without this the buffer is unbounded: a peer streaming
        // bytes that never contain `8=` would grow it for the whole session, and
        // `MAX_BODY_LENGTH` does not cover this path — it bounds a *declared*
        // length, and here there is no header to declare one.
        //
        // One byte is not junk worth reporting, and an empty buffer is the
        // ordinary idle case (`AlwaysSpin` drains every cycle), so neither is
        // `Malformed` — that would log on every quiet pass.
        return if buf.len() > 1 {
            Frame::Malformed {
                error: FrameError::Header,
                consumed: buf.len() - 1,
            }
        } else {
            Frame::Incomplete
        };
    };
    if start > 0 {
        return Frame::Malformed {
            error: FrameError::Header,
            consumed: start,
        };
    }

    // `8=<ver>\x019=<len>\x01` — locate the BodyLength value.
    let Some(first_soh) = buf.iter().position(|&b| b == SOH) else {
        return Frame::Incomplete;
    };
    let len_field = first_soh + 1;
    // Not enough bytes to tell whether tag 9 is there — that is a short read, not
    // a malformed frame.
    if buf.len() < len_field + 2 {
        return Frame::Incomplete;
    }
    if !buf[len_field..].starts_with(b"9=") {
        // Not a BodyLength where one is required. Resync past this `8=`.
        return Frame::Malformed {
            error: FrameError::Header,
            consumed: resync_from(buf, 2),
        };
    }
    let len_start = len_field + 2;
    let Some(len_soh_off) = buf[len_start..].iter().position(|&b| b == SOH) else {
        return Frame::Incomplete;
    };
    let len_soh = len_start + len_soh_off;
    let Some(body_len) = std::str::from_utf8(&buf[len_start..len_soh])
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        return Frame::Malformed {
            error: FrameError::Header,
            consumed: resync_from(buf, 2),
        };
    };
    if body_len > MAX_BODY_LENGTH {
        return Frame::Malformed {
            error: FrameError::BodyLength,
            consumed: resync_from(buf, 2),
        };
    }

    // BodyLength counts the bytes between the SOH after tag 9 and the `10=` of
    // the trailer, so the trailer starts at exactly that offset.
    let body_start = len_soh + 1;
    let Some(trailer) = body_start.checked_add(body_len) else {
        return Frame::Malformed {
            error: FrameError::BodyLength,
            consumed: resync_from(buf, 2),
        };
    };
    // Need `10=` plus at least one digit and the terminating SOH.
    if buf.len() < trailer + 5 {
        return Frame::Incomplete;
    }
    if !buf[trailer..].starts_with(b"10=") {
        return Frame::Malformed {
            error: FrameError::BodyLength,
            consumed: resync_from(buf, 2),
        };
    }
    let chk_start = trailer + 3;
    let Some(chk_soh_off) = buf[chk_start..].iter().position(|&b| b == SOH) else {
        return Frame::Incomplete;
    };
    let end = chk_start + chk_soh_off + 1;

    let declared = std::str::from_utf8(&buf[chk_start..chk_start + chk_soh_off])
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    let computed = u32::from(buf[..trailer].iter().fold(0u8, |a, &b| a.wrapping_add(b)));
    if declared != Some(computed) {
        return Frame::Malformed {
            error: FrameError::CheckSum,
            // The frame length itself is trustworthy (BodyLength landed on the
            // trailer), so drop exactly this message and carry on.
            consumed: end,
        };
    }

    Frame::Message {
        bytes: buf[..end].to_vec(),
        consumed: end,
    }
}

/// Bytes to drop so the next `find_message` starts at a fresh `8=`: everything
/// up to the next one after `from`, or the whole buffer if there is none.
fn resync_from(buf: &[u8], from: usize) -> usize {
    next_begin_string(buf, from).unwrap_or(buf.len())
}

/// Drain every complete FIX message from `parse_buf`, dispatching session-level
/// messages and pushing application/status events into `events`.
///
/// A garbled frame is logged and skipped, and the buffer resynchronised to the
/// next `8=`, with no reply — see [`find_message`] for why the spec forbids
/// Rejecting one. A frame that framed and checksummed cleanly but carries no
/// `MsgType` *is* Rejected: that one names a sequence number we can trust. The
/// socket is dropped (set to `None`) if the session must terminate — a sequence
/// divergence the protocol has no recovery for, or an unresponsive counterparty.
///
/// Returns `true` if any events were pushed.
fn drain_parse_buf<W: Write>(
    parse_buf: &mut Vec<u8>,
    socket: &mut Option<W>,
    session: &mut FixSession,
    events: &mut Burst<FixEvent>,
    is_acceptor: bool,
) -> anyhow::Result<bool> {
    let before = events.len();
    loop {
        match find_message(parse_buf) {
            Frame::Incomplete => break,
            Frame::Malformed { error, consumed } => {
                parse_buf.drain(..consumed.min(parse_buf.len()));
                // Garbled: dropped, never answered. `find_message` documents why.
                log::warn!("fix: dropped a garbled frame: {}", error.text());
                if consumed == 0 {
                    // Defensive: never spin on a frame we cannot advance past.
                    break;
                }
            }
            Frame::Message { bytes, consumed } => {
                parse_buf.drain(..consumed);
                let Some(msg) = build_message(decode_fields(&bytes)) else {
                    // Framed and checksum-clean but with no MsgType. Unlike a
                    // garbled frame this one is trustworthy enough to answer —
                    // though its own MsgSeqNum is the field that is missing, so
                    // RefSeqNum stays 0, the conventional "not applicable".
                    if let Some(sock) = socket.as_mut() {
                        session.send_reject(
                            sock,
                            0,
                            REJECT_REQUIRED_TAG_MISSING,
                            "missing MsgType (35)",
                        )?;
                    }
                    continue;
                };
                let mut sock = match socket.take() {
                    Some(s) => s,
                    None => continue,
                };
                let dispatch = handle_session(session, &msg, &mut sock, events, is_acceptor)?;
                match dispatch {
                    Dispatch::Deliver => {
                        *socket = Some(sock);
                        events.push(FixEvent::Data(msg));
                    }
                    Dispatch::Consumed => *socket = Some(sock),
                    // Leave `socket` as `None`: the caller sees the session is
                    // gone and closes the connection.
                    Dispatch::Terminate => break,
                }
            }
        }
    }
    Ok(events.len() > before)
}

// ── FixSession ────────────────────────────────────────────────────────────────

struct FixSession {
    sender_comp_id: String,
    target_comp_id: String,
    out_seq: u64,
    /// Next inbound `MsgSeqNum` (tag 34) expected. FIX sequences start at 1.
    in_seq: u64,
    /// How the Logon message authenticates (password, custom signer, or none).
    logon: FixLogon,
    /// Negotiated `HeartBtInt` in seconds — [`HEARTBEAT_INTERVAL`] for an
    /// initiator, or whatever the initiator asked for once an acceptor has seen
    /// its Logon.
    heartbeat: Duration,
    /// When we last wrote anything to the socket, for the outbound heartbeat.
    last_sent: Instant,
    /// When we last read a valid message, for liveness detection.
    last_recv: Instant,
    /// Set when a `TestRequest` has been sent and not yet answered; cleared by
    /// any inbound message. Gates the disconnect decision so a single slow
    /// heartbeat does not drop the session.
    test_request_outstanding: bool,
    /// The expected `MsgSeqNum` an outstanding `ResendRequest` was sent for, if
    /// any. Keyed on the *expected* number rather than the received one, because
    /// the expectation does not move during a gap — so every later message past
    /// the same gap finds the request already outstanding instead of firing
    /// another one.
    resend_requested_from: Option<u64>,
    /// Where the sequence numbers are kept across reconnects.
    store: SeqNumFile,
    /// Application messages retained for replay on an inbound `ResendRequest`.
    /// Empty and inert under [`FixMessageStore::None`], the default.
    sent_store: SentStore,
    /// Outbound bytes a non-blocking socket has not accepted yet: the unwritten
    /// suffix of a partially written frame, followed by any whole frames encoded
    /// since. Drained before anything else is written, so the wire always
    /// carries whole frames in sequence order. Empty on a blocking socket, which
    /// never reports `WouldBlock`.
    pending_out: Vec<u8>,
}

/// What [`FixSession::validate_sequence`] decided about an inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SeqDecision {
    /// In sequence — dispatch it.
    Process,
    /// Past the expected number: a gap. A `ResendRequest` has been sent (or was
    /// already outstanding) and this message is dropped; the counterparty will
    /// resend the missing range and this one after it.
    Gap { expected: u64, received: u64 },
    /// Already seen, and flagged `PossDupFlag=Y` — a legitimate resend of
    /// something we processed. Ignore it without advancing.
    DuplicateIgnored,
    /// Below the expected number *without* `PossDupFlag`. FIX 4.4 requires the
    /// session be terminated: the counterparty's view of the sequence has
    /// diverged from ours and there is no defined recovery.
    Fatal(String),
}

/// Ceiling on [`FixSession::pending_out`]. A counterparty that stops reading
/// entirely must surface as an error rather than as unbounded memory growth,
/// so the buffer is capped at roughly a thousand full-size frames.
const MAX_PENDING_OUT: usize = 8 * 1024 * 1024;

/// How long a teardown Logout may wait on a backpressured socket before being
/// abandoned. Best-effort and explicitly bounded: this runs in `Drop`, where
/// blocking indefinitely would hang the process at shutdown.
const TEARDOWN_FLUSH: Duration = Duration::from_millis(100);

impl FixSession {
    fn new(sender: &str, target: &str) -> Self {
        Self::new_with_logon(sender, target, FixLogon::None)
    }

    fn new_with_logon(sender: &str, target: &str, logon: FixLogon) -> Self {
        Self::build(
            sender,
            target,
            logon,
            &FixSeqNumStore::Reset,
            Duration::from_secs(u64::from(HEARTBEAT_INTERVAL)),
        )
    }

    fn build(
        sender: &str,
        target: &str,
        logon: FixLogon,
        store: &FixSeqNumStore,
        heartbeat: Duration,
    ) -> Self {
        Self::build_with_message_store(
            sender,
            target,
            logon,
            store,
            &FixMessageStore::None,
            heartbeat,
        )
    }

    fn build_with_message_store(
        sender: &str,
        target: &str,
        logon: FixLogon,
        store: &FixSeqNumStore,
        message_store: &FixMessageStore,
        heartbeat: Duration,
    ) -> Self {
        let now = Instant::now();
        let mut store = SeqNumFile::open(store);
        let (out_seq, in_seq) = store.load();
        Self {
            sender_comp_id: sender.to_string(),
            target_comp_id: target.to_string(),
            out_seq,
            in_seq,
            logon,
            heartbeat,
            last_sent: now,
            last_recv: now,
            test_request_outstanding: false,
            resend_requested_from: None,
            store,
            sent_store: SentStore::new(message_store),
            pending_out: Vec::new(),
        }
    }

    /// Push whatever the socket will take of [`pending_out`](Self::pending_out),
    /// returning `true` once the buffer is empty. A `WouldBlock` leaves the
    /// remainder queued and returns `false` — the caller carries on with its
    /// cycle rather than spinning, and the next write completes the frame.
    ///
    /// Never blocks and never busy-waits: at most one short write syscall per
    /// call once the socket is full.
    fn flush_pending<W: Write>(&mut self, sock: &mut W) -> anyhow::Result<bool> {
        // The common case, and the one the spin cycle hits every cycle: nothing
        // queued, no syscall, no work.
        if self.pending_out.is_empty() {
            return Ok(true);
        }
        let mut written = 0usize;
        let outcome = loop {
            if written == self.pending_out.len() {
                break Ok(true);
            }
            match sock.write(&self.pending_out[written..]) {
                Ok(0) => break Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break Ok(false),
                Err(e) => break Err(e.into()),
            }
        };
        self.pending_out.drain(..written);
        if outcome.as_ref().is_ok_and(|done| *done) {
            // The socket took everything, so flushing it can only help a wrapper
            // that buffers (TLS); a bare `TcpStream` flush is a no-op.
            sock.flush()?;
        }
        outcome
    }

    /// Queue one whole frame and push as much of the queue as the socket takes.
    ///
    /// The frame is appended to [`pending_out`](Self::pending_out) *before* the
    /// write, so a frame is never interleaved with a later one and never
    /// truncated: what the counterparty sees is a prefix of the byte stream we
    /// intended, never a corruption of it.
    fn write_frame<W: Write>(&mut self, sock: &mut W, bytes: &[u8]) -> anyhow::Result<()> {
        if self.pending_out.len() + bytes.len() > MAX_PENDING_OUT {
            anyhow::bail!(
                "FIX outbound buffer exceeded {MAX_PENDING_OUT} bytes with {} still queued: \
                 the counterparty has stopped reading",
                self.pending_out.len()
            );
        }
        self.pending_out.extend_from_slice(bytes);
        self.flush_pending(sock)?;
        Ok(())
    }

    /// Drain [`pending_out`](Self::pending_out) for up to `budget`, yielding
    /// between attempts. For teardown only — every on-graph path defers to the
    /// next cycle instead of waiting.
    fn flush_pending_within<W: Write>(&mut self, sock: &mut W, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            match self.flush_pending(sock) {
                Ok(true) => return true,
                Ok(false) => {}
                Err(_) => return false,
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
    }

    /// Adopt the counterparty's requested `HeartBtInt` from its Logon. An
    /// acceptor must honour whatever the initiator asks for; `0` disables
    /// heartbeating entirely, which the spec permits.
    fn adopt_heartbeat_interval(&mut self, msg: &FixMessage) {
        if let Some(secs) = msg.field(TAG_HEARTBT_INT).and_then(|v| v.parse().ok()) {
            self.heartbeat = Duration::from_secs(secs);
        }
    }

    /// Reset both sequence numbers, as a `ResetSeqNumFlag=Y` Logon requires.
    ///
    /// Only correct for the side that has not yet sent under the new sequence:
    /// the initiator calls it *before* writing its own Logon. For a reset the
    /// counterparty asked for, use [`apply_inbound_reset`](Self::apply_inbound_reset).
    fn reset_sequences(&mut self) {
        self.out_seq = 0;
        self.in_seq = 1;
        self.resend_requested_from = None;
        // The retained messages were filed under numbers that no longer mean
        // anything; replaying them after a reset would answer a resend request
        // with the wrong messages.
        self.sent_store.clear();
        self.store.save(self.out_seq, self.in_seq);
    }

    /// Apply the `ResetSeqNumFlag=Y` on an **inbound** Logon.
    ///
    /// The inbound expectation always restarts at 1 — that is what the flag
    /// says. The *outbound* sequence is only ours to restart when we have not
    /// already sent a message under it, which is exactly the acceptor's case: it
    /// replies to the Logon, so its reply must go out as 1.
    ///
    /// An initiator sent its Logon **first**, so zeroing here would make the
    /// next message reuse the number that Logon already consumed and the venue
    /// would see a too-low `MsgSeqNum` — a Logout, per the rule this adapter
    /// itself implements. That bites both stores: with
    /// [`FixSeqNumStore::Reset`] the initiator reset and sent Logon #1, and the
    /// acceptor's confirming `ResetSeqNumFlag=Y` would take it back to 0; with
    /// [`FixSeqNumStore::File`] it resumed a sequence a unilateral venue reset
    /// would discard.
    fn apply_inbound_reset(&mut self, is_acceptor: bool) {
        if is_acceptor {
            self.out_seq = 0;
        }
        self.in_seq = 1;
        self.resend_requested_from = None;
        self.store.save(self.out_seq, self.in_seq);
    }

    /// Check an inbound message's `MsgSeqNum` against what we expect, advancing
    /// the expected number and emitting a `ResendRequest` as required.
    ///
    /// Called for every inbound message **except** those whose whole purpose is
    /// to establish or repair the sequence (`Logon` with `ResetSeqNumFlag`, and
    /// `SequenceReset`), which the dispatchers handle before calling here.
    fn validate_sequence<W: Write>(
        &mut self,
        msg: &FixMessage,
        sock: &mut W,
        events: &mut Burst<FixEvent>,
    ) -> anyhow::Result<SeqDecision> {
        self.last_recv = Instant::now();
        self.test_request_outstanding = false;
        let received = msg.seq_num;

        if received == self.in_seq {
            self.in_seq += 1;
            // In sequence again: whatever gap we asked about is closed.
            self.resend_requested_from = None;
            self.store.save(self.out_seq, self.in_seq);
            return Ok(SeqDecision::Process);
        }

        if received > self.in_seq {
            let expected = self.in_seq;
            // Ask once per gap: a ResendRequest stays outstanding until the
            // counterparty's replay closes it, so later messages past the same
            // gap must not fire another.
            if self.resend_requested_from != Some(expected) {
                self.resend_requested_from = Some(expected);
                // EndSeqNo=0 means "everything from BeginSeqNo onwards".
                self.send(
                    sock,
                    MSG_RESEND_REQUEST,
                    &[
                        (TAG_BEGIN_SEQ_NO, expected.to_string()),
                        (TAG_END_SEQ_NO, "0".to_string()),
                    ],
                )?;
            }
            events.push(FixEvent::Status(FixSessionStatus::SequenceGap {
                expected,
                received,
            }));
            return Ok(SeqDecision::Gap { expected, received });
        }

        // received < in_seq.
        if msg.field(TAG_POSS_DUP_FLAG) == Some("Y") {
            return Ok(SeqDecision::DuplicateIgnored);
        }
        Ok(SeqDecision::Fatal(format!(
            "MsgSeqNum too low: expected {}, received {} without PossDupFlag=Y",
            self.in_seq, received
        )))
    }

    /// Send a session-level `Reject` (MsgType 3) naming the offending message.
    fn send_reject<W: Write>(
        &mut self,
        sock: &mut W,
        ref_seq_num: u64,
        reason: u32,
        text: &str,
    ) -> anyhow::Result<()> {
        self.send(
            sock,
            MSG_REJECT,
            &[
                (TAG_REF_SEQ_NUM, ref_seq_num.to_string()),
                (TAG_SESSION_REJECT_REASON, reason.to_string()),
                (TAG_TEXT, text.to_string()),
            ],
        )
    }

    /// Answer an inbound `ResendRequest` over `[begin, end]` (`end == 0` meaning
    /// "everything from `begin` onwards").
    ///
    /// With a [`FixMessageStore::Memory`] store the retained application
    /// messages in the range are **replayed** — each at its original
    /// `MsgSeqNum` and `SendingTime`, with `PossDupFlag=Y` and
    /// `OrigSendingTime` added, as FIX 4.4 requires — and only the ranges that
    /// were admin messages or have aged out of the ring are covered by
    /// `SequenceReset`-`GapFill`. With the default [`FixMessageStore::None`] the
    /// whole range is one gap fill, which is what this did before the store
    /// existed.
    ///
    /// Neither a replayed message nor a gap fill consumes a new outbound
    /// sequence number: each carries the number it is standing in for. Taking a
    /// fresh one would create the very gap the answer is closing.
    fn answer_resend_request<W: Write>(
        &mut self,
        sock: &mut W,
        begin: u64,
        end: u64,
    ) -> anyhow::Result<()> {
        // `EndSeqNo = 0` is FIX's "and everything after"; the last number we
        // actually sent bounds it.
        let end = if end == 0 {
            self.out_seq
        } else {
            end.min(self.out_seq)
        };
        if begin > end {
            // Nothing in range — the counterparty is asking for messages we
            // never sent. One gap fill over the request keeps it moving.
            return self.send_gap_fill(sock, begin, begin.max(end) + 1);
        }

        // Which of the requested numbers we can actually replay. Collected up
        // front so the borrow of `sent_store` ends before the writes.
        let replay: Vec<(u64, String, String, Vec<(u32, String)>)> = self
            .sent_store
            .range(begin, end)
            .map(|m| {
                (
                    m.seq,
                    m.msg_type.clone(),
                    m.sending_time.clone(),
                    m.fields.clone(),
                )
            })
            .collect();

        let mut next = begin;
        for (seq, msg_type, sending_time, fields) in replay {
            // Admin messages and evicted ones sit between the replays; one gap
            // fill covers each such run.
            if seq > next {
                self.send_gap_fill(sock, next, seq)?;
            }
            let mut extra = fields;
            extra.push((TAG_POSS_DUP_FLAG, "Y".to_string()));
            extra.push((TAG_ORIG_SENDING_TIME, sending_time));
            // The resend carries a *fresh* SendingTime; the original moves to
            // tag 122. That is what lets the counterparty tell a replay from
            // the first transmission.
            let now = now_sending_time();
            self.write_at_seq(sock, &msg_type, seq, &now, &extra)?;
            next = seq + 1;
        }
        if next <= end {
            self.send_gap_fill(sock, next, end + 1)?;
        }
        Ok(())
    }

    /// Emit one `SequenceReset`-`GapFill` covering `[from, new_seq_no)`.
    ///
    /// It carries `MsgSeqNum = from` — the first number it is standing in for —
    /// and does not consume an outbound number of its own.
    fn send_gap_fill<W: Write>(
        &mut self,
        sock: &mut W,
        from: u64,
        new_seq_no: u64,
    ) -> anyhow::Result<()> {
        let sending_time = now_sending_time();
        self.write_at_seq(
            sock,
            MSG_SEQUENCE_RESET,
            from,
            &sending_time,
            &[
                (TAG_GAP_FILL_FLAG, "Y".to_string()),
                (TAG_POSS_DUP_FLAG, "Y".to_string()),
                (TAG_NEW_SEQ_NO, new_seq_no.to_string()),
            ],
        )
    }

    /// Apply an inbound `SequenceReset`, in either `GapFill` or `Reset` mode.
    ///
    /// `NewSeqNo` below what we already expect is rejected rather than applied —
    /// moving the expected sequence backwards would make us reprocess messages.
    fn apply_sequence_reset<W: Write>(
        &mut self,
        msg: &FixMessage,
        sock: &mut W,
    ) -> anyhow::Result<()> {
        let Some(new_seq_no) = msg
            .field(TAG_NEW_SEQ_NO)
            .and_then(|v| v.parse::<u64>().ok())
        else {
            return self.send_reject(
                sock,
                msg.seq_num,
                REJECT_VALUE_OUT_OF_RANGE,
                "SequenceReset without a usable NewSeqNo",
            );
        };
        if new_seq_no < self.in_seq {
            return self.send_reject(
                sock,
                msg.seq_num,
                REJECT_VALUE_OUT_OF_RANGE,
                "NewSeqNo would move the expected sequence backwards",
            );
        }
        self.in_seq = new_seq_no;
        self.resend_requested_from = None;
        self.last_recv = Instant::now();
        self.test_request_outstanding = false;
        self.store.save(self.out_seq, self.in_seq);
        Ok(())
    }

    /// Drive the heartbeat clock: send a `Heartbeat` when we have been quiet for
    /// the interval, a `TestRequest` when the *counterparty* has, and report when
    /// it has been quiet long enough to be considered gone.
    ///
    /// Called from both session loops on every pass — the `AlwaysSpin` cycle and
    /// the `Threaded` loop's read-timeout wakeup — so neither needs its own
    /// timer.
    fn maintain<W: Write>(&mut self, sock: &mut W) -> anyhow::Result<Liveness> {
        // HeartBtInt=0 disables heartbeating in both directions.
        if self.heartbeat.is_zero() {
            return Ok(Liveness::Alive);
        }
        let now = Instant::now();

        let quiet_inbound = now.duration_since(self.last_recv).as_secs_f64();
        let interval = self.heartbeat.as_secs_f64();
        if quiet_inbound >= interval * DISCONNECT_AFTER && self.test_request_outstanding {
            return Ok(Liveness::Unresponsive);
        }
        if quiet_inbound >= interval * TEST_REQUEST_AFTER && !self.test_request_outstanding {
            self.test_request_outstanding = true;
            self.send(
                sock,
                MSG_TEST_REQUEST,
                &[(TAG_TEST_REQ_ID, format!("wf-{}", self.out_seq + 1))],
            )?;
            return Ok(Liveness::Alive);
        }

        if now.duration_since(self.last_sent) >= self.heartbeat {
            self.send_heartbeat(sock, None)?;
        }
        Ok(Liveness::Alive)
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
        self.write_frame(sock, &bytes)?;
        // Any write resets the outbound heartbeat clock, and the sequence number
        // it consumed must be durable before the counterparty can act on it.
        self.last_sent = Instant::now();
        self.store.save(self.out_seq, self.in_seq);
        // The single choke point every outbound message passes through, so the
        // one place retention can be complete. Admin messages and a disabled
        // store both fall out inside `record`.
        self.sent_store
            .record(self.out_seq, msg_type, sending_time, extra);
        Ok(())
    }

    /// Write a message at an **explicit** sequence number, consuming none.
    ///
    /// Resends and gap fills both need this: a replayed message carries the
    /// `MsgSeqNum` it originally had, and a `SequenceReset`-`GapFill` carries the
    /// first number of the range it is filling. Neither may take a fresh number —
    /// doing so would itself create the gap it is trying to close.
    fn write_at_seq<W: Write>(
        &mut self,
        sock: &mut W,
        msg_type: &str,
        seq: u64,
        sending_time: &str,
        extra: &[(u32, String)],
    ) -> anyhow::Result<()> {
        let bytes = encode_message(
            msg_type,
            &self.sender_comp_id,
            &self.target_comp_id,
            seq,
            sending_time,
            extra,
        );
        self.write_frame(sock, &bytes)?;
        self.last_sent = Instant::now();
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

    /// An **initiator's** Logon.
    ///
    /// With a persistent store we resume the sequence and must *not* ask for a
    /// reset — asking would discard exactly the continuity the store exists to
    /// provide, and many venues rate-limit or refuse an unsolicited reset. With
    /// no store there is nothing to resume, so we ask for the reset and apply it
    /// locally too, so both sides agree from the first message.
    fn send_logon<W: Write>(&mut self, sock: &mut W) -> anyhow::Result<()> {
        let reset = self.store.resets_on_logon;
        if reset {
            self.reset_sequences();
        }
        self.write_logon(sock, reset)
    }

    /// An **acceptor's** Logon reply, confirming the flag the initiator sent.
    ///
    /// Deliberately does not touch the sequence numbers: by the time we reply,
    /// the initiator's Logon has already been validated and consumed at its
    /// sequence, and a reset here would put `in_seq` back to expecting it again.
    /// Any reset the initiator asked for was applied before that validation.
    fn send_logon_reply<W: Write>(&mut self, sock: &mut W, echo_reset: bool) -> anyhow::Result<()> {
        self.write_logon(sock, echo_reset)
    }

    fn write_logon<W: Write>(&mut self, sock: &mut W, reset_flag: bool) -> anyhow::Result<()> {
        let logon = self.logon.clone();
        let sender_id = self.sender_comp_id.clone();
        let target_id = self.target_comp_id.clone();
        let heartbeat = self.heartbeat.as_secs();
        let reset = if reset_flag { "Y" } else { "N" };
        self.send_with(sock, MSG_LOGON, move |seq, sending_time| {
            let mut extra = vec![
                (TAG_ENCRYPT_METHOD, "0".to_string()),
                (TAG_HEARTBT_INT, heartbeat.to_string()),
                (TAG_RESET_SEQ_NUM_FLAG, reset.to_string()),
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

    /// Logout carrying a `Text` (tag 58) explanation — sent before terminating a
    /// session whose sequence has diverged, so the counterparty's logs say why.
    fn send_logout_with_reason<W: Write>(
        &mut self,
        sock: &mut W,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.send(sock, MSG_LOGOUT, &[(TAG_TEXT, reason.to_string())])
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

/// What the session dispatcher decided to do with an inbound message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    /// Forward it to the application layer.
    Deliver,
    /// Handled at the session level (or deliberately dropped); nothing to
    /// forward.
    Consumed,
    /// The session cannot continue. An `Error` status has been pushed; the
    /// caller must close the connection.
    Terminate,
}

/// Handle one inbound message for either role.
///
/// The ordering here is the protocol's, and it matters:
///
/// 1. **`SequenceReset`** is processed regardless of its own `MsgSeqNum` — its
///    whole purpose is to repair a sequence that is already wrong, so validating
///    it against the sequence it is fixing would deadlock recovery. (FIX 4.4 §
///    "SequenceReset-Reset".)
/// 2. **`Logon`** establishes the sequence, and a `ResetSeqNumFlag=Y` Logon
///    *defines* it as 1, so the reset is applied before validation.
/// 3. Everything else is sequence-validated **before** it is dispatched, so a
///    gap is detected whatever the message type — including on the application
///    messages that carry fills.
fn handle_session<W: Write>(
    session: &mut FixSession,
    msg: &FixMessage,
    sock: &mut W,
    events: &mut Burst<FixEvent>,
    is_acceptor: bool,
) -> anyhow::Result<Dispatch> {
    // (1) SequenceReset — sequence-exempt by design.
    if msg.msg_type == MSG_SEQUENCE_RESET {
        session.apply_sequence_reset(msg, sock)?;
        return Ok(Dispatch::Consumed);
    }

    // (2) Logon — may redefine the sequence before we can validate against it.
    if msg.msg_type == MSG_LOGON {
        session.adopt_heartbeat_interval(msg);
        let asked_reset = msg.field(TAG_RESET_SEQ_NUM_FLAG) == Some("Y");
        if asked_reset {
            session.apply_inbound_reset(is_acceptor);
        }
        let decision = session.validate_sequence(msg, sock, events)?;
        if let SeqDecision::Fatal(why) = decision {
            // Same courtesy as the general path below: say why before going.
            let _ = session.send_logout_with_reason(sock, &why);
            events.push(FixEvent::Status(FixSessionStatus::Error(why)));
            return Ok(Dispatch::Terminate);
        }
        // An acceptor answers the Logon; an initiator's Logon was already sent.
        if is_acceptor {
            session.send_logon_reply(sock, asked_reset)?;
        }
        events.push(FixEvent::Status(FixSessionStatus::LoggedIn));
        return Ok(Dispatch::Consumed);
    }

    // (3) Everything else: validate first.
    match session.validate_sequence(msg, sock, events)? {
        SeqDecision::Process => {}
        // The gap's ResendRequest is out; this message returns in order.
        SeqDecision::Gap { .. } | SeqDecision::DuplicateIgnored => {
            return Ok(Dispatch::Consumed);
        }
        SeqDecision::Fatal(why) => {
            // Spec: tell the counterparty why, then terminate.
            let _ = session.send_logout_with_reason(sock, &why);
            events.push(FixEvent::Status(FixSessionStatus::Error(why)));
            return Ok(Dispatch::Terminate);
        }
    }

    match msg.msg_type.as_str() {
        MSG_HEARTBEAT => Ok(Dispatch::Consumed),
        MSG_TEST_REQUEST => {
            // Echo TestReqID (tag 112) so the counterparty can correlate.
            let id = msg.field(TAG_TEST_REQ_ID).map(str::to_string);
            session.send_heartbeat(sock, id)?;
            Ok(Dispatch::Consumed)
        }
        MSG_RESEND_REQUEST => {
            let begin = msg
                .field(TAG_BEGIN_SEQ_NO)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1);
            let end = msg
                .field(TAG_END_SEQ_NO)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            session.answer_resend_request(sock, begin, end)?;
            Ok(Dispatch::Consumed)
        }
        MSG_LOGOUT => {
            let reason = msg.field(TAG_TEXT).map(str::to_string);
            events.push(FixEvent::Status(FixSessionStatus::LoggedOut(reason)));
            Ok(Dispatch::Consumed)
        }
        // `Reject` (3) is deliberately *not* consumed: a session-level reject of
        // something we sent is information the application needs — it is how a
        // venue tells you an order was malformed.
        _ => Ok(Dispatch::Deliver),
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
/// wingfoil twin of legacy's `split_events`. Each half keeps the burst grouping:
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
/// legacy's run-`start()` "real-time" check, moved earlier to wiring.
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
    seq_num_store: FixSeqNumStore,
    /// See [`FixOptions::message_store`].
    message_store: FixMessageStore,
    heartbeat: Duration,
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
    /// (legacy emits it from `start`, which the wingfoil start hook cannot).
    logging_in_pending: bool,
}

impl SpinState {
    fn new(cfg: &FixConfig) -> Self {
        Self {
            is_acceptor: cfg.is_acceptor,
            session: FixSession::build_with_message_store(
                &cfg.sender_comp_id,
                &cfg.target_comp_id,
                cfg.logon.clone(),
                &cfg.seq_num_store,
                &cfg.message_store,
                cfg.heartbeat,
            ),
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

        // Complete any frame the socket only partially accepted before anything
        // else is written. This is the cycle the write path defers to, and the
        // reason it can defer rather than spin: the read phase above still runs
        // while a stalled socket drains.
        if let Some(sock) = self.socket.as_mut()
            && let Err(e) = self.session.flush_pending(sock)
        {
            self.socket = None;
            events.push(FixEvent::Status(FixSessionStatus::Error(e.to_string())));
            return Ok(events);
        }

        // Heartbeat maintenance runs every cycle: this mode has no other timer,
        // and without it the session advertises a HeartBtInt it never honours.
        if let Some(sock) = self.socket.as_mut() {
            match self.session.maintain(sock) {
                Ok(Liveness::Alive) => {}
                Ok(Liveness::Unresponsive) => {
                    self.socket = None;
                    events.push(FixEvent::Status(FixSessionStatus::Error(
                        "counterparty unresponsive: no data and no answer to TestRequest".into(),
                    )));
                }
                // A failed heartbeat write means the socket is gone.
                Err(e) => {
                    self.socket = None;
                    events.push(FixEvent::Status(FixSessionStatus::Error(e.to_string())));
                }
            }
        }
        Ok(events)
    }
}

/// Drops the `AlwaysSpin` session cleanly at teardown: send a best-effort Logout
/// on the live socket (legacy's `stop`).
struct SpinLogoutGuard(Rc<RefCell<SpinState>>);

impl Drop for SpinLogoutGuard {
    fn drop(&mut self) {
        let mut st = self.0.borrow_mut();
        let sock = st.socket.take();
        if let Some(mut sock) = sock {
            let _ = st.session.send_logout(&mut sock);
            // The Logout may have queued behind a backpressured socket, and there
            // is no next cycle to drain it on. Wait briefly, but never past
            // TEARDOWN_FLUSH: this is a destructor.
            let _ = st.session.flush_pending_within(&mut sock, TEARDOWN_FLUSH);
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
            // run at start with context — legacy's `start` `?`.
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

        let mut session = FixSession::build_with_message_store(
            &cfg.sender_comp_id,
            &cfg.target_comp_id,
            cfg.logon.clone(),
            &cfg.seq_num_store,
            &cfg.message_store,
            cfg.heartbeat,
        );

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

        // Heartbeat maintenance. The read timeout above bounds how long we can go
        // without reaching here, so `THREADED_READ_TIMEOUT` is also the timer's
        // resolution — 200 ms against a 30 s interval.
        match sock_opt.as_mut().map(|s| session.maintain(s)) {
            None | Some(Ok(Liveness::Alive)) => {}
            Some(Ok(Liveness::Unresponsive)) => {
                let _ = chan.send(FixEvent::Status(FixSessionStatus::Error(
                    "counterparty unresponsive: no data and no answer to TestRequest".into(),
                )));
                // Reconnect rather than give up: an established session that went
                // quiet is exactly the case `Threaded` reconnect exists for.
                return true;
            }
            Some(Err(_)) => return true,
        }

        // The dispatcher clears the socket when the session must terminate.
        if sock_opt.is_none() {
            for event in events {
                if !chan.send(event) {
                    return false;
                }
            }
            return true;
        }

        for event in events {
            if !chan.send(event) {
                return false;
            }
        }
    }
}

// ── Public factory functions ──────────────────────────────────────────────────

/// Optional session settings for the `*_with_options` factories.
///
/// Every field has a default matching what the plain factories use, so
/// `FixOptions::default()` is exactly the base behaviour and you only name what
/// you want to change via the `with_*` setters — the struct is
/// `#[non_exhaustive]` so new options can be added without a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FixOptions {
    /// Where sequence numbers live between connections. Defaults to
    /// [`FixSeqNumStore::Reset`] — see that type for why a production order-flow
    /// session usually wants [`FixSeqNumStore::File`] instead.
    pub seq_num_store: FixSeqNumStore,
    /// Whether sent application messages are retained so an inbound
    /// `ResendRequest` can be answered with the real messages. Defaults to
    /// [`FixMessageStore::None`] — the gap-fill answer — so an existing
    /// session's conversation with a venue is unchanged. Order-flow sessions
    /// generally want [`FixMessageStore::Memory`].
    pub message_store: FixMessageStore,
    /// `HeartBtInt` (tag 108) to offer in the Logon, in seconds. `0` disables
    /// heartbeating in both directions. An **acceptor** ignores this and adopts
    /// whatever the initiator asks for, as the spec requires.
    pub heartbeat_secs: u32,
}

impl Default for FixOptions {
    fn default() -> Self {
        Self {
            seq_num_store: FixSeqNumStore::Reset,
            message_store: FixMessageStore::None,
            heartbeat_secs: HEARTBEAT_INTERVAL,
        }
    }
}

impl FixOptions {
    /// Replace where sequence numbers live between connections.
    #[must_use]
    pub fn with_seq_num_store(mut self, store: FixSeqNumStore) -> Self {
        self.seq_num_store = store;
        self
    }

    /// Replace how sent application messages are retained for resend.
    #[must_use]
    pub fn with_message_store(mut self, store: FixMessageStore) -> Self {
        self.message_store = store;
        self
    }

    /// Replace the `HeartBtInt` (tag 108) offered in the Logon, in seconds.
    #[must_use]
    pub fn with_heartbeat_secs(mut self, heartbeat_secs: u32) -> Self {
        self.heartbeat_secs = heartbeat_secs;
        self
    }

    fn heartbeat(&self) -> Duration {
        Duration::from_secs(u64::from(self.heartbeat_secs))
    }
}

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
    fix_connect_with_options(
        g,
        run_mode,
        host,
        port,
        sender_comp_id,
        target_comp_id,
        mode,
        FixOptions::default(),
    )
}

/// [`fix_connect`] with explicit session settings — a persistent sequence-number
/// store, a non-default `HeartBtInt`, or both. See [`FixOptions`].
#[allow(clippy::too_many_arguments)]
pub fn fix_connect_with_options(
    g: &GraphBuilder,
    run_mode: RunMode,
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
    options: FixOptions,
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
        heartbeat: options.heartbeat(),
        seq_num_store: options.seq_num_store,
        message_store: options.message_store.clone(),
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
    fix_accept_with_options(
        g,
        run_mode,
        port,
        sender_comp_id,
        target_comp_id,
        mode,
        FixOptions::default(),
    )
}

/// [`fix_accept`] with explicit session settings. See [`FixOptions`] — note that
/// an acceptor adopts the initiator's `HeartBtInt`, so only the sequence-number
/// store is meaningful here.
pub fn fix_accept_with_options(
    g: &GraphBuilder,
    run_mode: RunMode,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
    options: FixOptions,
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
        heartbeat: options.heartbeat(),
        seq_num_store: options.seq_num_store,
        message_store: options.message_store.clone(),
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
    fix_connect_tls_logon_with_options(
        g,
        run_mode,
        host,
        port,
        sender_comp_id,
        target_comp_id,
        logon,
        FixOptions::default(),
    )
}

/// [`fix_connect_tls_logon`] with explicit session settings. See [`FixOptions`].
///
/// This is the factory a production order-flow session wants: pair it with
/// [`FixSeqNumStore::File`] so a reconnect resumes the sequence instead of
/// resetting it, which is what makes gap detection survive a drop.
#[allow(clippy::too_many_arguments)]
pub fn fix_connect_tls_logon_with_options(
    g: &GraphBuilder,
    run_mode: RunMode,
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    logon: FixLogon,
    options: FixOptions,
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
        heartbeat: options.heartbeat(),
        seq_num_store: options.seq_num_store,
        message_store: options.message_store.clone(),
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
        sending_time: wingfoil::NanoTime::ZERO,
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
/// best-effort Logout (legacy's `stop`).
struct FixSenderGuard(Rc<RefCell<FixSenderState>>);

impl Drop for FixSenderGuard {
    fn drop(&mut self) {
        let mut st = self.0.borrow_mut();
        let sock = st.socket.take();
        if let Some(mut sock) = sock {
            let _ = st.session.send_logout(&mut sock);
            // Same bounded best-effort as the spin guard: give a backpressured
            // socket a moment to take the Logout, then give up rather than hang
            // teardown.
            let _ = st.session.flush_pending_within(&mut sock, TEARDOWN_FLUSH);
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
            // so a historical run aborts naming the run mode (legacy ordering).
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

    // ── helpers ──────────────────────────────────────────────────────────────

    /// A stand-in for a non-blocking socket with a bounded send buffer: it takes
    /// at most `budget` more bytes, then reports `WouldBlock` until the peer
    /// drains it. Both behaviours are ones `Write::write_all` does not survive —
    /// a short write, and a full buffer mid-frame.
    #[derive(Default)]
    struct BackpressuredWriter {
        bytes: Vec<u8>,
        writes: usize,
        /// Bytes the socket will still accept before its buffer is full. `None`
        /// once the peer has drained it and it accepts everything again.
        budget: Option<usize>,
    }

    impl BackpressuredWriter {
        /// A socket with room for `budget` more bytes and no more.
        fn with_room_for(budget: usize) -> Self {
            Self {
                budget: Some(budget),
                ..Self::default()
            }
        }

        /// The counterparty read: the send buffer is empty again.
        fn drained(&mut self) {
            self.budget = None;
        }
    }

    impl Write for BackpressuredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            match self.budget {
                None => {
                    self.bytes.extend_from_slice(buf);
                    Ok(buf.len())
                }
                Some(0) => Err(io::Error::from(io::ErrorKind::WouldBlock)),
                Some(room) => {
                    let accepted = buf.len().min(room);
                    self.bytes.extend_from_slice(&buf[..accepted]);
                    self.budget = Some(room - accepted);
                    Ok(accepted)
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Frame one complete message, asserting it framed cleanly.
    fn frame(bytes: &[u8]) -> Vec<u8> {
        match find_message(bytes) {
            Frame::Message { bytes, .. } => bytes,
            other => panic!("expected a complete message, got {other:?}"),
        }
    }

    /// Build an inbound message the way the counterparty would — through the
    /// real encoder and decoder, so tests exercise the codec rather than
    /// hand-assembling a `FixMessage` the wire could never produce.
    fn inbound(msg_type: &str, seq: u64, extra: &[(u32, &str)]) -> FixMessage {
        let owned: Vec<(u32, String)> = extra.iter().map(|(t, v)| (*t, v.to_string())).collect();
        let bytes = encode_message(msg_type, "THEM", "US", seq, "20240627-11:17:25.223", &owned);
        build_message(decode_fields(&frame(&bytes))).expect("test message parses")
    }

    /// Decode everything a session wrote to its socket.
    fn sent(buf: &[u8]) -> Vec<FixMessage> {
        let mut out = Vec::new();
        let mut rest = buf;
        while let Frame::Message { bytes, consumed } = find_message(rest) {
            out.push(build_message(decode_fields(&bytes)).expect("sent message parses"));
            rest = &rest[consumed..];
        }
        out
    }

    /// A session already past logon: sequences at 1, ready for message 1.
    fn session() -> FixSession {
        let mut s = FixSession::new("US", "THEM");
        s.reset_sequences();
        s
    }

    /// Drive one inbound message through the session dispatcher, returning the
    /// dispatch decision, the bytes written, and the events raised.
    fn dispatch(
        session: &mut FixSession,
        msg: &FixMessage,
        is_acceptor: bool,
    ) -> (Dispatch, Vec<u8>, Burst<FixEvent>) {
        let mut sock = Vec::<u8>::new();
        let mut events: Burst<FixEvent> = TinyVec::new();
        let d = handle_session(session, msg, &mut sock, &mut events, is_acceptor)
            .expect("dispatch does not error");
        (d, sock, events)
    }

    fn statuses(events: &Burst<FixEvent>) -> Vec<FixSessionStatus> {
        events
            .iter()
            .filter_map(|e| match e {
                FixEvent::Status(s) => Some(s.clone()),
                FixEvent::Data(_) => None,
            })
            .collect()
    }

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
        let msg = build_message(decode_fields(&frame(&bytes))).expect("parse failed");
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
        let msg_bytes = frame(&buf);
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

    /// A full TCP send buffer must not turn a FIX message into a torn frame or
    /// an adapter error. The write returns having queued the unwritten suffix,
    /// and the next flush completes exactly one frame.
    #[test]
    fn outbound_write_defers_the_suffix_across_wouldblock() {
        let mut session = FixSession::new("ME", "YOU");
        let mut sock = BackpressuredWriter::with_room_for(7);

        session
            .send_heartbeat(&mut sock, None)
            .expect("backpressure is not an error");

        assert!(
            sent(&sock.bytes).is_empty(),
            "the peer has a frame prefix, not a whole message"
        );
        assert!(
            !session.pending_out.is_empty(),
            "the unwritten suffix is retained on the session"
        );

        // The write path never spins: one short write and one WouldBlock.
        assert_eq!(sock.writes, 2);

        sock.drained();
        assert!(
            session
                .flush_pending(&mut sock)
                .expect("the drained socket takes the suffix"),
            "nothing is left queued"
        );

        let messages = sent(&sock.bytes);
        assert_eq!(messages.len(), 1, "one complete frame reaches the peer");
        assert_eq!(messages[0].msg_type, MSG_HEARTBEAT);
    }

    /// Messages sent while a suffix is still queued must land *behind* it. The
    /// counterparty sees whole frames in sequence order, never one spliced into
    /// the middle of another.
    #[test]
    fn queued_frames_never_overtake_a_partial_one() {
        let mut session = FixSession::new("ME", "YOU");
        let mut sock = BackpressuredWriter::with_room_for(7);

        session.send_heartbeat(&mut sock, None).expect("first send");
        session
            .send_heartbeat(&mut sock, None)
            .expect("second send, while the first is still queued");

        sock.drained();
        assert!(
            session.flush_pending(&mut sock).expect("socket drained"),
            "both frames flushed"
        );

        let messages = sent(&sock.bytes);
        assert_eq!(messages.len(), 2, "two complete frames, in order");
        assert_eq!(
            messages.iter().map(|m| m.seq_num).collect::<Vec<_>>(),
            vec![1, 2],
            "sequence order is preserved across the stall"
        );
    }

    /// A counterparty that never reads must surface as an error rather than as
    /// unbounded memory growth.
    #[test]
    fn outbound_buffer_is_capped() {
        let mut session = FixSession::new("ME", "YOU");
        let mut sock = BackpressuredWriter::with_room_for(0);

        session.pending_out = vec![b'0'; MAX_PENDING_OUT];
        let err = session
            .send_heartbeat(&mut sock, None)
            .expect_err("the cap is enforced");

        assert!(
            err.to_string().contains("stopped reading"),
            "the error names the cause, got: {err}"
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
        let msg_bytes = frame(&buf);
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

    // ── codec: framing, checksum, timestamps ─────────────────────────────────

    /// The headline framing bug. A length-delimited field (95/96 `RawData`) may
    /// contain arbitrary bytes — SOH included — and FIX handles that by framing
    /// on BodyLength. The old scan-for-`\x0110=` implementation split this
    /// message in the middle of its payload.
    #[test]
    fn framing_uses_body_length_not_a_trailer_scan() {
        let payload = "\x0110=999\x01embedded";
        let bytes = encode_message(
            "8",
            "SENDER",
            "TARGET",
            7,
            "20240627-11:17:25.223",
            &[
                (95, payload.len().to_string()),
                (96, payload.to_string()),
                (37, "ORDER-1".to_string()),
            ],
        );
        // One message in, one message out — not two, and not a truncated one.
        let framed = frame(&bytes);
        assert_eq!(framed.len(), bytes.len());
        let msg = build_message(decode_fields(&framed)).expect("parses");
        assert_eq!(msg.msg_type, "8");
        assert_eq!(msg.seq_num, 7);
        // The field after the embedded trailer survived, which is the proof the
        // frame was not cut short.
        assert_eq!(msg.field(37), Some("ORDER-1"));
    }

    #[test]
    fn a_corrupt_checksum_is_rejected_rather_than_accepted() {
        let bytes = encode_message("0", "S", "T", 1, "20240627-11:17:25.223", &[]);
        let mut corrupt = bytes.clone();
        // Rewrite the checksum digits to something that cannot be right.
        let len = corrupt.len();
        corrupt[len - 4..len - 1].copy_from_slice(b"000");
        match find_message(&corrupt) {
            Frame::Malformed { error, consumed } => {
                assert_eq!(error, FrameError::CheckSum);
                // The frame length was trustworthy, so exactly one message is dropped.
                assert_eq!(consumed, corrupt.len());
            }
            other => panic!("expected BadCheckSum, got {other:?}"),
        }
        // And the intact original still frames.
        assert_eq!(frame(&bytes).len(), bytes.len());
    }

    /// An *understated* BodyLength is detectable at once: the trailer is not
    /// where tag 9 says it is. (An overstated one is indistinguishable from a
    /// short read until more bytes arrive — see
    /// `an_overstated_body_length_reads_as_incomplete`.)
    #[test]
    fn a_body_length_that_misses_the_trailer_is_rejected() {
        let bytes = encode_message("0", "S", "T", 1, "20240627-11:17:25.223", &[]);
        let text = String::from_utf8_lossy(&bytes).to_string();
        let broken = text.replacen("\u{1}9=45\u{1}", "\u{1}9=10\u{1}", 1);
        assert_ne!(broken, text, "fixture must actually rewrite BodyLength");
        match find_message(broken.as_bytes()) {
            Frame::Malformed { error, .. } => assert_eq!(error, FrameError::BodyLength),
            other => panic!("expected BadBodyLength, got {other:?}"),
        }
    }

    /// An overstated BodyLength looks exactly like a message still in flight, so
    /// the framer waits rather than guessing. `MAX_BODY_LENGTH` is what stops
    /// that wait from being unbounded.
    #[test]
    fn an_overstated_body_length_reads_as_incomplete() {
        let bytes = encode_message("0", "S", "T", 1, "20240627-11:17:25.223", &[]);
        let text = String::from_utf8_lossy(&bytes).to_string();
        let broken = text.replacen("\u{1}9=45\u{1}", "\u{1}9=4500\u{1}", 1);
        assert!(matches!(find_message(broken.as_bytes()), Frame::Incomplete));
    }

    #[test]
    fn an_absurd_body_length_cannot_make_the_session_buffer_forever() {
        let broken = format!("8=FIX.4.4\x019={}\x0135=0\x01", MAX_BODY_LENGTH + 1);
        match find_message(broken.as_bytes()) {
            Frame::Malformed { error, .. } => assert_eq!(error, FrameError::BodyLength),
            other => panic!("expected BadBodyLength, got {other:?}"),
        }
    }

    #[test]
    fn junk_before_the_begin_string_resynchronises() {
        let good = encode_message("0", "S", "T", 1, "20240627-11:17:25.223", &[]);
        let mut buf = b"garbage bytes ".to_vec();
        buf.extend_from_slice(&good);
        match find_message(&buf) {
            Frame::Malformed { consumed, .. } => {
                // Drops exactly the junk, leaving the good message at the head.
                assert_eq!(consumed, 14);
                assert_eq!(frame(&buf[consumed..]).len(), good.len());
            }
            other => panic!("expected a resync, got {other:?}"),
        }
    }

    #[test]
    fn a_partial_message_waits_for_more_bytes() {
        let bytes = encode_message("0", "S", "T", 1, "20240627-11:17:25.223", &[]);
        for cut in 1..bytes.len() {
            match find_message(&bytes[..cut]) {
                Frame::Incomplete => {}
                other => panic!("prefix of {cut} bytes should be incomplete, got {other:?}"),
            }
        }
    }

    #[test]
    fn two_messages_in_one_read_both_frame() {
        let mut buf = encode_message("0", "S", "T", 1, "20240627-11:17:25.223", &[]);
        let second = encode_message("1", "S", "T", 2, "20240627-11:17:26.223", &[]);
        let first_len = buf.len();
        buf.extend_from_slice(&second);
        match find_message(&buf) {
            Frame::Message { consumed, .. } => assert_eq!(consumed, first_len),
            other => panic!("expected the first message, got {other:?}"),
        }
        assert_eq!(frame(&buf[first_len..]).len(), second.len());
    }

    #[test]
    fn sending_time_is_parsed_off_the_wire() {
        let msg = inbound("0", 1, &[]);
        assert_ne!(
            msg.sending_time,
            wingfoil::NanoTime::ZERO,
            "SendingTime should no longer be hardcoded to zero"
        );
        let expected =
            chrono::NaiveDateTime::parse_from_str("20240627-11:17:25.223", "%Y%m%d-%H:%M:%S%.f")
                .map(wingfoil::NanoTime::from)
                .expect("fixture parses");
        assert_eq!(msg.sending_time, expected);
    }

    #[test]
    fn sending_time_accepts_every_precision_venues_send() {
        let base = "20240627-11:17:25";
        for suffix in ["", ".2", ".223", ".223456", ".223456789"] {
            let value = format!("{base}{suffix}");
            assert!(
                parse_sending_time(&value).is_some(),
                "should parse SendingTime {value}"
            );
        }
        assert_eq!(parse_sending_time("not-a-timestamp"), None);
    }

    #[test]
    fn an_unparseable_sending_time_does_not_cost_the_message() {
        let owned = vec![(37u32, "ORDER-1".to_string())];
        let bytes = encode_message("8", "THEM", "US", 4, "rubbish", &owned);
        let msg = build_message(decode_fields(&frame(&bytes))).expect("still parses");
        assert_eq!(msg.sending_time, wingfoil::NanoTime::ZERO);
        assert_eq!(msg.field(37), Some("ORDER-1"));
    }

    // ── codec: repeating groups ──────────────────────────────────────────────

    /// A two-sided MarketDataSnapshot — the shape `fix_sub` itself requests, and
    /// the one `field()` alone cannot read: it returns only the bid's price.
    fn market_data_snapshot() -> FixMessage {
        inbound(
            "W",
            1,
            &[
                (55, "EUR/USD"),
                (268, "2"),
                (269, "0"),
                (270, "1.0850"),
                (271, "1000000"),
                (269, "1"),
                (270, "1.0852"),
                (271, "2000000"),
            ],
        )
    }

    #[test]
    fn groups_split_a_snapshot_into_correlated_entries() {
        let msg = market_data_snapshot();
        // What the flat accessor gives you: the first entry only.
        assert_eq!(msg.field(270), Some("1.0850"));

        let entries = msg.groups(268, 269);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].field(269), Some("0")); // bid
        assert_eq!(entries[0].field(270), Some("1.0850"));
        assert_eq!(entries[0].field(271), Some("1000000"));
        assert_eq!(entries[1].field(269), Some("1")); // offer
        assert_eq!(entries[1].field(270), Some("1.0852"));
        assert_eq!(entries[1].field(271), Some("2000000"));
    }

    #[test]
    fn a_group_entry_does_not_see_its_neighbours_fields() {
        let msg = market_data_snapshot();
        let entries = msg.groups(268, 269);
        // Scoping is the whole point: entry 0 must not resolve entry 1's price.
        assert_eq!(entries[0].fields().len(), 3);
        assert!(
            entries[0].field(55).is_none(),
            "symbol is outside the group"
        );
    }

    #[test]
    fn fields_all_returns_every_value_for_a_repeated_tag() {
        let msg = market_data_snapshot();
        assert_eq!(
            msg.fields_all(270).collect::<Vec<_>>(),
            vec!["1.0850", "1.0852"]
        );
        assert_eq!(msg.fields_all(999).count(), 0);
    }

    #[test]
    fn groups_are_empty_without_a_usable_count() {
        // No count tag at all.
        let msg = inbound("W", 1, &[(269, "0"), (270, "1.0")]);
        assert!(msg.groups(268, 269).is_empty());
        // Count of zero.
        let msg = inbound("W", 1, &[(268, "0")]);
        assert!(msg.groups(268, 269).is_empty());
        assert_eq!(msg.group_count(268), Some(0));
    }

    #[test]
    fn group_entries_are_capped_at_the_declared_count() {
        // Three delimiters but a declared count of two: trust the count.
        let msg = inbound("W", 1, &[(268, "2"), (269, "0"), (269, "1"), (269, "2")]);
        assert_eq!(msg.groups(268, 269).len(), 2);
    }

    #[test]
    fn a_delimiter_valued_tag_before_the_count_is_not_a_group_entry() {
        let msg = inbound("W", 1, &[(269, "9"), (268, "1"), (269, "0"), (270, "1.0")]);
        let entries = msg.groups(268, 269);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].field(269), Some("0"));
    }

    // ── session: sequence numbers ────────────────────────────────────────────

    #[test]
    fn an_in_sequence_message_is_delivered_and_advances_the_expectation() {
        let mut s = session();
        let (d, out, events) = dispatch(&mut s, &inbound("8", 1, &[(37, "O-1")]), false);
        assert_eq!(d, Dispatch::Deliver);
        assert_eq!(s.in_seq, 2);
        assert!(out.is_empty(), "nothing to send for an in-sequence message");
        assert!(statuses(&events).is_empty());
    }

    /// The defect that mattered most: a gap used to pass through silently, so a
    /// missed ExecutionReport was undetectable.
    #[test]
    fn a_gap_sends_a_resend_request_and_reports_it() {
        let mut s = session();
        // Expecting 1, receiving 5 — messages 1..=4 were missed.
        let (d, out, events) = dispatch(&mut s, &inbound("8", 5, &[(37, "O-5")]), false);
        assert_eq!(
            d,
            Dispatch::Consumed,
            "the message must not be delivered out of order"
        );
        assert_eq!(s.in_seq, 1, "the expectation does not advance across a gap");

        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_RESEND_REQUEST);
        assert_eq!(written[0].field(TAG_BEGIN_SEQ_NO), Some("1"));
        assert_eq!(written[0].field(TAG_END_SEQ_NO), Some("0"));

        assert_eq!(
            statuses(&events),
            vec![FixSessionStatus::SequenceGap {
                expected: 1,
                received: 5
            }]
        );
    }

    #[test]
    fn a_gap_asks_for_a_resend_only_once() {
        let mut s = session();
        let (_, first, _) = dispatch(&mut s, &inbound("8", 5, &[]), false);
        assert_eq!(sent(&first).len(), 1);
        // A second message still past the gap must not trigger a second request.
        let (_, second, _) = dispatch(&mut s, &inbound("8", 6, &[]), false);
        assert!(
            sent(&second).is_empty(),
            "a persistent gap should ask once, not per message"
        );
    }

    #[test]
    fn a_resent_duplicate_is_ignored_when_poss_dup_is_set() {
        let mut s = session();
        dispatch(&mut s, &inbound("8", 1, &[]), false);
        dispatch(&mut s, &inbound("8", 2, &[]), false);
        assert_eq!(s.in_seq, 3);
        // The counterparty replays message 1 with PossDupFlag=Y.
        let (d, out, _) = dispatch(&mut s, &inbound("8", 1, &[(TAG_POSS_DUP_FLAG, "Y")]), false);
        assert_eq!(d, Dispatch::Consumed);
        assert_eq!(s.in_seq, 3, "a duplicate must not move the expectation");
        assert!(out.is_empty());
    }

    #[test]
    fn a_low_sequence_without_poss_dup_terminates_the_session() {
        let mut s = session();
        dispatch(&mut s, &inbound("8", 1, &[]), false);
        dispatch(&mut s, &inbound("8", 2, &[]), false);
        // Sequence 1 again, with no PossDupFlag: unrecoverable per FIX 4.4.
        let (d, out, events) = dispatch(&mut s, &inbound("8", 1, &[]), false);
        assert_eq!(d, Dispatch::Terminate);
        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_LOGOUT);
        assert!(
            written[0]
                .field(TAG_TEXT)
                .unwrap_or_default()
                .contains("too low"),
            "the Logout should say why: {:?}",
            written[0].field(TAG_TEXT)
        );
        assert!(matches!(
            statuses(&events).first(),
            Some(FixSessionStatus::Error(_))
        ));
    }

    #[test]
    fn a_sequence_reset_gap_fill_advances_the_expectation() {
        let mut s = session();
        let (d, out, _) = dispatch(
            &mut s,
            &inbound(
                MSG_SEQUENCE_RESET,
                1,
                &[(TAG_GAP_FILL_FLAG, "Y"), (TAG_NEW_SEQ_NO, "10")],
            ),
            false,
        );
        assert_eq!(d, Dispatch::Consumed);
        assert_eq!(s.in_seq, 10);
        assert!(sent(&out).is_empty(), "a valid reset needs no reply");
    }

    /// A SequenceReset exists to repair a broken sequence, so validating it
    /// against the sequence it is repairing would deadlock recovery.
    #[test]
    fn a_sequence_reset_is_applied_even_when_its_own_seqnum_is_wrong() {
        let mut s = session();
        s.in_seq = 50;
        let (d, _, _) = dispatch(
            &mut s,
            &inbound(MSG_SEQUENCE_RESET, 1, &[(TAG_NEW_SEQ_NO, "77")]),
            false,
        );
        assert_eq!(d, Dispatch::Consumed);
        assert_eq!(s.in_seq, 77);
    }

    #[test]
    fn a_sequence_reset_that_would_go_backwards_is_rejected() {
        let mut s = session();
        s.in_seq = 50;
        let (_, out, _) = dispatch(
            &mut s,
            &inbound(MSG_SEQUENCE_RESET, 1, &[(TAG_NEW_SEQ_NO, "10")]),
            false,
        );
        assert_eq!(s.in_seq, 50, "must not reprocess messages we have seen");
        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_REJECT);
        assert_eq!(
            written[0].field(TAG_SESSION_REJECT_REASON),
            Some(REJECT_VALUE_OUT_OF_RANGE.to_string().as_str())
        );
    }

    #[test]
    fn an_inbound_resend_request_is_answered_with_a_gap_fill() {
        let mut s = session();
        let (d, out, _) = dispatch(
            &mut s,
            &inbound(
                MSG_RESEND_REQUEST,
                1,
                &[(TAG_BEGIN_SEQ_NO, "1"), (TAG_END_SEQ_NO, "0")],
            ),
            false,
        );
        assert_eq!(d, Dispatch::Consumed);
        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_SEQUENCE_RESET);
        assert_eq!(written[0].field(TAG_GAP_FILL_FLAG), Some("Y"));
        // The gap fill stands in for the range from the requested start, so it
        // carries that number rather than consuming a fresh one.
        assert_eq!(1, written[0].seq_num);
        assert_eq!(written[0].field(TAG_POSS_DUP_FLAG), Some("Y"));
    }

    /// A session with a message store open, already past logon.
    fn session_with_store(capacity: usize) -> FixSession {
        let mut s = FixSession::build_with_message_store(
            "US",
            "THEM",
            FixLogon::None,
            &FixSeqNumStore::Reset,
            &FixMessageStore::Memory { capacity },
            Duration::from_secs(u64::from(HEARTBEAT_INTERVAL)),
        );
        s.reset_sequences();
        s
    }

    /// Send `n` application messages (`NewOrderSingle`), returning the session.
    fn send_orders(s: &mut FixSession, n: usize) {
        let mut sink = Vec::<u8>::new();
        for i in 0..n {
            s.send(&mut sink, "D", &[(TAG_TEXT, format!("order-{i}"))])
                .expect("send");
        }
    }

    /// The point of the store: the counterparty gets its **orders** back, not a
    /// gap fill telling it to forget them.
    #[test]
    fn a_resend_request_replays_the_stored_application_messages() {
        let mut s = session_with_store(16);
        send_orders(&mut s, 3); // seq 1, 2, 3

        let (d, out, _) = dispatch(
            &mut s,
            &inbound(
                MSG_RESEND_REQUEST,
                1,
                &[(TAG_BEGIN_SEQ_NO, "1"), (TAG_END_SEQ_NO, "0")],
            ),
            false,
        );
        assert_eq!(d, Dispatch::Consumed);

        let written = sent(&out);
        assert_eq!(3, written.len(), "all three orders came back");
        for (i, msg) in written.iter().enumerate() {
            let seq = i as u64 + 1;
            assert_eq!("D", msg.msg_type, "a real order, not a SequenceReset");
            assert_eq!(seq, msg.seq_num, "replayed at its original MsgSeqNum");
            assert_eq!(
                Some(format!("order-{i}").as_str()),
                msg.field(TAG_TEXT),
                "the original payload"
            );
            assert_eq!(
                Some("Y"),
                msg.field(TAG_POSS_DUP_FLAG),
                "a resend must be flagged PossDup"
            );
            assert!(
                msg.field(TAG_ORIG_SENDING_TIME).is_some(),
                "a resend must carry OrigSendingTime"
            );
        }
        // Replaying consumed no outbound sequence numbers.
        assert_eq!(3, s.out_seq);
    }

    /// Only the *requested* range comes back.
    #[test]
    fn a_resend_request_honours_its_begin_and_end() {
        let mut s = session_with_store(16);
        send_orders(&mut s, 5); // seq 1..=5

        let (_, out, _) = dispatch(
            &mut s,
            &inbound(
                MSG_RESEND_REQUEST,
                1,
                &[(TAG_BEGIN_SEQ_NO, "2"), (TAG_END_SEQ_NO, "4")],
            ),
            false,
        );
        let written = sent(&out);
        assert_eq!(
            vec![2, 3, 4],
            written.iter().map(|m| m.seq_num).collect::<Vec<u64>>()
        );
    }

    /// Admin messages are not replayed — they are gap-filled — so a range that
    /// straddles one comes back as orders either side of a `SequenceReset`.
    #[test]
    fn admin_messages_in_the_range_are_gap_filled_between_the_replays() {
        let mut s = session_with_store(16);
        let mut sink = Vec::<u8>::new();
        s.send(&mut sink, "D", &[(TAG_TEXT, "first".to_string())])
            .expect("send"); // seq 1, app
        s.send(&mut sink, MSG_HEARTBEAT, &[]).expect("send"); // seq 2, admin
        s.send(&mut sink, "D", &[(TAG_TEXT, "second".to_string())])
            .expect("send"); // seq 3, app

        let (_, out, _) = dispatch(
            &mut s,
            &inbound(
                MSG_RESEND_REQUEST,
                1,
                &[(TAG_BEGIN_SEQ_NO, "1"), (TAG_END_SEQ_NO, "0")],
            ),
            false,
        );
        let written = sent(&out);
        assert_eq!(3, written.len());

        assert_eq!(("D", 1), (written[0].msg_type.as_str(), written[0].seq_num));
        // The heartbeat's slot is filled, not replayed.
        assert_eq!(MSG_SEQUENCE_RESET, written[1].msg_type);
        assert_eq!(
            2, written[1].seq_num,
            "the gap fill starts where the gap does"
        );
        assert_eq!(Some("Y"), written[1].field(TAG_GAP_FILL_FLAG));
        assert_eq!(
            Some("3"),
            written[1].field(TAG_NEW_SEQ_NO),
            "and points at the next real message"
        );
        assert_eq!(("D", 3), (written[2].msg_type.as_str(), written[2].seq_num));
    }

    /// Messages that have aged out of a small ring are gap-filled; the ones
    /// still held are replayed. A bounded store degrades, it does not lie.
    #[test]
    fn messages_evicted_from_the_ring_are_gap_filled() {
        let mut s = session_with_store(2);
        send_orders(&mut s, 4); // seq 1..=4; only 3 and 4 survive

        let (_, out, _) = dispatch(
            &mut s,
            &inbound(
                MSG_RESEND_REQUEST,
                1,
                &[(TAG_BEGIN_SEQ_NO, "1"), (TAG_END_SEQ_NO, "0")],
            ),
            false,
        );
        let written = sent(&out);
        assert_eq!(3, written.len());
        assert_eq!(MSG_SEQUENCE_RESET, written[0].msg_type);
        assert_eq!(1, written[0].seq_num);
        assert_eq!(
            Some("3"),
            written[0].field(TAG_NEW_SEQ_NO),
            "the fill covers exactly the evicted range"
        );
        assert_eq!(("D", 3), (written[1].msg_type.as_str(), written[1].seq_num));
        assert_eq!(("D", 4), (written[2].msg_type.as_str(), written[2].seq_num));
    }

    /// A sequence reset invalidates the numbers the store filed messages under,
    /// so it must drop them rather than answer a later request with the wrong
    /// messages.
    #[test]
    fn a_sequence_reset_drops_the_retained_messages() {
        let mut s = session_with_store(16);
        send_orders(&mut s, 3);
        assert_eq!(3, s.sent_store.sent.len());

        s.reset_sequences();
        assert!(s.sent_store.sent.is_empty());
    }

    /// The default is unchanged: no store, so the whole range is one gap fill
    /// and no application message is retained.
    #[test]
    fn without_a_store_nothing_is_retained() {
        let mut s = session();
        send_orders(&mut s, 3);
        assert!(s.sent_store.sent.is_empty());
        assert!(!s.sent_store.enabled());
    }

    #[test]
    fn a_test_request_is_answered_with_a_heartbeat_echoing_the_id() {
        let mut s = session();
        let (d, out, _) = dispatch(
            &mut s,
            &inbound(MSG_TEST_REQUEST, 1, &[(TAG_TEST_REQ_ID, "PING-42")]),
            false,
        );
        assert_eq!(d, Dispatch::Consumed);
        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_HEARTBEAT);
        assert_eq!(written[0].field(TAG_TEST_REQ_ID), Some("PING-42"));
    }

    /// A session-level Reject is how a venue tells you an order was malformed —
    /// it must reach the application, not be swallowed as session plumbing.
    #[test]
    fn a_reject_is_delivered_to_the_application() {
        let mut s = session();
        let (d, _, _) = dispatch(
            &mut s,
            &inbound(
                MSG_REJECT,
                1,
                &[(TAG_REF_SEQ_NUM, "9"), (TAG_TEXT, "bad tag")],
            ),
            false,
        );
        assert_eq!(d, Dispatch::Deliver);
    }

    #[test]
    fn a_logon_with_the_reset_flag_restarts_the_sequence() {
        let mut s = session();
        s.in_seq = 99;
        let (d, _, events) = dispatch(
            &mut s,
            &inbound(MSG_LOGON, 1, &[(TAG_RESET_SEQ_NUM_FLAG, "Y")]),
            false,
        );
        assert_eq!(d, Dispatch::Consumed);
        assert_eq!(s.in_seq, 2, "reset to 1, then message 1 consumed");
        assert_eq!(statuses(&events), vec![FixSessionStatus::LoggedIn]);
    }

    /// Regression: the acceptor's Logon *reply* must not reset the sequence it
    /// has just consumed, or it goes back to expecting the Logon again.
    #[test]
    fn an_acceptor_replies_to_logon_without_re_resetting_the_sequence() {
        let mut s = FixSession::new("US", "THEM");
        let (d, out, events) = dispatch(
            &mut s,
            &inbound(
                MSG_LOGON,
                1,
                &[(TAG_RESET_SEQ_NUM_FLAG, "Y"), (TAG_HEARTBT_INT, "30")],
            ),
            true,
        );
        assert_eq!(d, Dispatch::Consumed);
        assert_eq!(s.in_seq, 2, "the acceptor consumed Logon #1 and expects #2");
        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_LOGON);
        assert_eq!(written[0].field(TAG_RESET_SEQ_NUM_FLAG), Some("Y"));
        assert_eq!(statuses(&events), vec![FixSessionStatus::LoggedIn]);

        // And the next application message is accepted in sequence.
        let (d, _, _) = dispatch(&mut s, &inbound("8", 2, &[]), true);
        assert_eq!(d, Dispatch::Deliver);
    }

    /// The acceptor's own outbound sequence *does* restart on an inbound reset:
    /// it replies to the Logon, so the reply must go out as #1.
    #[test]
    fn an_acceptor_restarts_its_outbound_sequence_on_an_inbound_reset() {
        let mut s = FixSession::new("US", "THEM");
        s.out_seq = 41; // as a resumed session would be
        dispatch(
            &mut s,
            &inbound(MSG_LOGON, 1, &[(TAG_RESET_SEQ_NUM_FLAG, "Y")]),
            true,
        );
        assert_eq!(s.out_seq, 1, "the Logon reply is message #1");
    }

    /// The mirror image, and a bug on the default path: an **initiator** sent
    /// its Logon *before* the reply arrives, so the confirming
    /// `ResetSeqNumFlag=Y` must not take its outbound sequence back to 0 — the
    /// next message would reuse the number that Logon already consumed, and the
    /// venue would see a too-low `MsgSeqNum` and log us out.
    #[test]
    fn an_initiator_keeps_its_outbound_sequence_across_the_logon_reply() {
        let mut s = FixSession::new("US", "THEM");
        let mut sock = Vec::<u8>::new();
        s.send_logon(&mut sock).expect("logon");
        assert_eq!(s.out_seq, 1, "our Logon went out as #1");

        // The acceptor confirms the reset in its reply.
        dispatch(
            &mut s,
            &inbound(MSG_LOGON, 1, &[(TAG_RESET_SEQ_NUM_FLAG, "Y")]),
            false,
        );
        assert_eq!(s.in_seq, 2, "their Logon #1 was consumed");
        assert_eq!(s.out_seq, 1, "ours is untouched — #1 is already spent");

        s.send(&mut sock, "D", &[]).expect("order");
        assert_eq!(s.out_seq, 2, "so the first order is #2, not a second #1");
    }

    /// A `File`-store initiator resumes a sequence a venue may still try to
    /// reset unilaterally. The inbound expectation obeys; the outbound one, which
    /// the resumed Logon has already drawn from, does not.
    #[test]
    fn a_resumed_initiator_does_not_rewind_its_outbound_sequence() {
        let mut s = FixSession::new("US", "THEM");
        s.out_seq = 41;
        s.in_seq = 12;
        dispatch(
            &mut s,
            &inbound(MSG_LOGON, 1, &[(TAG_RESET_SEQ_NUM_FLAG, "Y")]),
            false,
        );
        assert_eq!(s.in_seq, 2, "inbound restarts at 1, then consumes Logon #1");
        assert_eq!(s.out_seq, 41, "outbound keeps the sequence it resumed");
    }

    /// The Logon path terminates like every other: saying why before it goes.
    #[test]
    fn a_fatal_logon_sequence_sends_a_logout_naming_the_reason() {
        let mut s = session();
        s.in_seq = 10;
        // No reset flag, so the sequence is not redefined — and 3 is below 10
        // without PossDupFlag, which FIX 4.4 has no recovery for.
        let (d, out, events) = dispatch(&mut s, &inbound(MSG_LOGON, 3, &[]), false);
        assert_eq!(d, Dispatch::Terminate);
        let written = sent(&out);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_LOGOUT);
        assert!(
            written[0].field(TAG_TEXT).unwrap_or("").contains("too low"),
            "the Logout must name the reason: {:?}",
            written[0].field(TAG_TEXT)
        );
        assert!(matches!(
            statuses(&events).as_slice(),
            [FixSessionStatus::Error(_)]
        ));
    }

    #[test]
    fn an_acceptor_adopts_the_initiators_heartbeat_interval() {
        let mut s = FixSession::new("US", "THEM");
        dispatch(
            &mut s,
            &inbound(MSG_LOGON, 1, &[(TAG_HEARTBT_INT, "7")]),
            true,
        );
        assert_eq!(s.heartbeat, Duration::from_secs(7));
    }

    /// A garbled frame is dropped in silence. FIX 4.4 forbids Rejecting one: a
    /// Reject names its target by `RefSeqNum`, and a frame that failed its own
    /// CheckSum has no `MsgSeqNum` worth naming. Answering also amplifies — every
    /// Reject burns an outbound sequence number, so a corrupt stream would
    /// consume the session's sequence space at the rate the peer sends junk.
    #[test]
    fn a_garbled_frame_is_dropped_without_a_reply() {
        let mut s = session();
        let good = encode_message("0", "THEM", "US", 1, "20240627-11:17:25.223", &[]);
        let mut corrupt = good.clone();
        let len = corrupt.len();
        corrupt[len - 4..len - 1].copy_from_slice(b"000");

        let mut buf = corrupt;
        let mut sock = Some(Vec::<u8>::new());
        let mut events: Burst<FixEvent> = TinyVec::new();
        let before = s.out_seq;
        drain_parse_buf(&mut buf, &mut sock, &mut s, &mut events, false).expect("drains");

        assert!(
            sock.expect("socket survives").is_empty(),
            "a garbled frame must not be answered"
        );
        assert_eq!(s.out_seq, before, "and must not consume a sequence number");
        assert!(
            buf.is_empty(),
            "the bad frame is consumed, not left to loop on"
        );
    }

    /// The one frame that *is* trustworthy enough to Reject: it framed and
    /// checksummed cleanly, so only its contents are wrong.
    #[test]
    fn a_clean_frame_with_no_msg_type_is_rejected() {
        let mut s = session();
        // Encode with an empty MsgType, then strip the `35=` field outright so
        // the frame is well-formed but has no message type at all.
        let bytes = encode_message("0", "THEM", "US", 1, "20240627-11:17:25.223", &[]);
        let text = String::from_utf8_lossy(&bytes).to_string();
        let stripped = text.replacen("35=0\u{1}", "", 1);
        // Re-frame so BodyLength and CheckSum match the shortened body.
        let body_start = stripped.find("\u{1}").expect("header SOH") + 1;
        let body_start = stripped[body_start..].find("\u{1}").expect("9= SOH") + body_start + 1;
        let trailer = stripped.find("\u{1}10=").expect("trailer") + 1;
        let body = &stripped[body_start..trailer];
        let head = format!("8={BEGIN_STRING}\u{1}9={}\u{1}", body.len());
        let mut rebuilt = format!("{head}{body}").into_bytes();
        let sum = rebuilt.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        rebuilt.extend_from_slice(format!("10={sum:03}\u{1}").as_bytes());

        let mut buf = rebuilt;
        let mut sock = Some(Vec::<u8>::new());
        let mut events: Burst<FixEvent> = TinyVec::new();
        drain_parse_buf(&mut buf, &mut sock, &mut s, &mut events, false).expect("drains");

        let written = sent(&sock.expect("socket survives"));
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_REJECT);
        assert_eq!(
            written[0].field(TAG_SESSION_REJECT_REASON),
            Some(REJECT_REQUIRED_TAG_MISSING.to_string().as_str())
        );
    }

    /// Junk containing no `8=` at all must not accumulate. `MAX_BODY_LENGTH`
    /// does not cover this: it bounds a *declared* length, and here there is no
    /// header to declare one — so without an explicit drop, a peer streaming
    /// bytes that never begin a message grows the buffer for the whole session.
    #[test]
    fn junk_with_no_begin_string_does_not_grow_the_buffer() {
        let mut s = session();
        let mut buf = vec![b'x'; 8192];
        let mut sock = Some(Vec::<u8>::new());
        let mut events: Burst<FixEvent> = TinyVec::new();
        drain_parse_buf(&mut buf, &mut sock, &mut s, &mut events, false).expect("drains");
        assert_eq!(
            buf.len(),
            1,
            "all but the trailing byte — which may be a bare `8` — is dropped"
        );
        assert!(
            sock.expect("socket survives").is_empty(),
            "junk is garbled, so it is not answered either"
        );
    }

    /// The idle case, and the reason the drop above is not spelled `Malformed`
    /// unconditionally: `AlwaysSpin` drains every cycle, almost always with
    /// nothing buffered.
    #[test]
    fn an_empty_buffer_is_incomplete_not_garbled() {
        assert!(matches!(find_message(&[]), Frame::Incomplete));
        assert!(
            matches!(find_message(b"8"), Frame::Incomplete),
            "a lone `8` may be a begin-string whose `=` has not arrived"
        );
    }

    // ── session: heartbeats ──────────────────────────────────────────────────

    #[test]
    fn a_heartbeat_is_sent_once_the_session_has_been_quiet() {
        let mut s = session();
        s.heartbeat = Duration::from_secs(30);
        s.last_sent = Instant::now() - Duration::from_secs(31);
        let mut sock = Vec::<u8>::new();
        assert_eq!(s.maintain(&mut sock).expect("maintains"), Liveness::Alive);
        let written = sent(&sock);
        assert_eq!(
            written.len(),
            1,
            "the interval elapsed, so send a Heartbeat"
        );
        assert_eq!(written[0].msg_type, MSG_HEARTBEAT);
    }

    #[test]
    fn no_heartbeat_is_sent_while_the_session_is_busy() {
        let mut s = session();
        s.heartbeat = Duration::from_secs(30);
        let mut sock = Vec::<u8>::new();
        s.maintain(&mut sock).expect("maintains");
        assert!(sock.is_empty(), "nothing due yet");
    }

    #[test]
    fn a_quiet_counterparty_is_probed_then_declared_unresponsive() {
        let mut s = session();
        s.heartbeat = Duration::from_secs(10);
        let mut sock = Vec::<u8>::new();

        // Past TEST_REQUEST_AFTER (1.2 x 10s): probe it.
        s.last_recv = Instant::now() - Duration::from_secs(13);
        assert_eq!(s.maintain(&mut sock).expect("maintains"), Liveness::Alive);
        let written = sent(&sock);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].msg_type, MSG_TEST_REQUEST);
        assert!(written[0].field(TAG_TEST_REQ_ID).is_some());
        assert!(s.test_request_outstanding);

        // Still nothing back, and now past DISCONNECT_AFTER (2.4 x 10s).
        s.last_recv = Instant::now() - Duration::from_secs(25);
        assert_eq!(
            s.maintain(&mut sock).expect("maintains"),
            Liveness::Unresponsive
        );
    }

    #[test]
    fn an_answered_test_request_keeps_the_session_alive() {
        let mut s = session();
        s.heartbeat = Duration::from_secs(10);
        s.test_request_outstanding = true;
        s.last_recv = Instant::now() - Duration::from_secs(25);
        // Any inbound message clears the outstanding probe...
        dispatch(&mut s, &inbound(MSG_HEARTBEAT, 1, &[]), false);
        assert!(!s.test_request_outstanding);
        // ...so the session is not torn down.
        let mut sock = Vec::<u8>::new();
        assert_eq!(s.maintain(&mut sock).expect("maintains"), Liveness::Alive);
    }

    #[test]
    fn a_zero_heartbeat_interval_disables_heartbeating() {
        let mut s = session();
        s.heartbeat = Duration::ZERO;
        s.last_sent = Instant::now() - Duration::from_secs(3600);
        s.last_recv = Instant::now() - Duration::from_secs(3600);
        let mut sock = Vec::<u8>::new();
        assert_eq!(s.maintain(&mut sock).expect("maintains"), Liveness::Alive);
        assert!(sock.is_empty(), "HeartBtInt=0 means no heartbeats at all");
    }

    #[test]
    fn any_send_resets_the_outbound_heartbeat_clock() {
        let mut s = session();
        s.heartbeat = Duration::from_secs(30);
        s.last_sent = Instant::now() - Duration::from_secs(31);
        let mut sock = Vec::<u8>::new();
        s.send(&mut sock, "8", &[]).expect("sends");
        let before = sock.len();
        s.maintain(&mut sock).expect("maintains");
        assert_eq!(sock.len(), before, "the send already counted as traffic");
    }

    // ── session: sequence persistence ────────────────────────────────────────

    fn temp_store_path(tag: &str) -> std::path::PathBuf {
        // Unique per process and per test, so parallel runs never collide.
        let unique = format!(
            "wingfoil-fix-seq-{}-{tag}-{:p}.txt",
            std::process::id(),
            &tag
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn the_default_store_asks_the_venue_for_a_reset() {
        let mut s = FixSession::new("US", "THEM");
        let mut sock = Vec::<u8>::new();
        s.send_logon(&mut sock).expect("logon");
        let written = sent(&sock);
        assert_eq!(written[0].field(TAG_RESET_SEQ_NUM_FLAG), Some("Y"));
    }

    #[test]
    fn a_file_store_resumes_the_sequence_numbers_and_does_not_reset() {
        let path = temp_store_path("resume");
        let _ = std::fs::remove_file(&path);
        let store = FixSeqNumStore::File(path.clone());
        let heartbeat = Duration::from_secs(30);

        // First "connection": logon plus two application messages out, three in.
        {
            let mut s = FixSession::build("US", "THEM", FixLogon::None, &store, heartbeat);
            let mut sock = Vec::<u8>::new();
            s.send_logon(&mut sock).expect("logon");
            let written = sent(&sock);
            assert_eq!(
                written[0].field(TAG_RESET_SEQ_NUM_FLAG),
                Some("N"),
                "a persistent store must not discard the sequence it is keeping"
            );
            s.send(&mut sock, "D", &[]).expect("order");
            s.send(&mut sock, "D", &[]).expect("order");
            assert_eq!(s.out_seq, 3);
            for seq in 1..=3 {
                dispatch(&mut s, &inbound("8", seq, &[]), false);
            }
            assert_eq!(s.in_seq, 4);
        }

        // Second "connection" over the same file: both directions resume.
        {
            let mut s = FixSession::build("US", "THEM", FixLogon::None, &store, heartbeat);
            assert_eq!(s.out_seq, 3, "outbound sequence resumed");
            assert_eq!(s.in_seq, 4, "inbound expectation resumed");
            // And an in-sequence message at the resumed number is accepted.
            let (d, _, _) = dispatch(&mut s, &inbound("8", 4, &[]), false);
            assert_eq!(d, Dispatch::Deliver);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_store_file_starts_at_the_fix_beginning() {
        let path = temp_store_path("fresh");
        let _ = std::fs::remove_file(&path);
        let s = FixSession::build(
            "US",
            "THEM",
            FixLogon::None,
            &FixSeqNumStore::File(path.clone()),
            Duration::from_secs(30),
        );
        assert_eq!((s.out_seq, s.in_seq), (0, 1));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unopenable_store_warns_and_falls_back_to_reset() {
        // A directory that does not exist cannot be opened as a file.
        let path = std::path::PathBuf::from("/nonexistent-wingfoil-dir/seq.txt");
        let mut s = FixSession::build(
            "US",
            "THEM",
            FixLogon::None,
            &FixSeqNumStore::File(path),
            Duration::from_secs(30),
        );
        let mut sock = Vec::<u8>::new();
        s.send_logon(&mut sock).expect("logon still establishes");
        let written = sent(&sock);
        assert_eq!(
            written[0].field(TAG_RESET_SEQ_NUM_FLAG),
            Some("Y"),
            "degrade visibly to reset rather than silently claiming continuity"
        );
    }
}
