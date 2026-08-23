//! ws adapter — a reconnecting WebSocket **client** transport.
//!
//! There is no legacy twin: this is new surface, defined for the streaming
//! venue adapters that normalise into [`market`](crate::adapters::market).
//! Those adapters are all the same shape underneath — open a socket, send a
//! subscribe payload, read frames until the venue drops you, back off, connect
//! again, *send the subscribe payload again* — and that loop is where they are
//! most alike and most likely to be got subtly wrong. This module owns it once
//! so each venue crate is left with only its own parsing.
//!
//! It is deliberately **payload-agnostic**: it yields raw [`WsMessage`] frames
//! and knows nothing about JSON, venue envelopes or [`market`](crate::adapters::market)
//! types. Layer the parsing above it.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`statistics`](crate::adapters::statistics)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Sources** — the free builder functions [`ws_sub`] (frames only) and
//!   [`ws_connect`] (frames + on-graph [`WsStatus`] + an outbound
//!   [`WsSender`]) on a [`GraphBuilder`].
//! - **Sink** — the [`WsSinkOps`] extension trait, enabled with
//!   `use wingfoil::adapters::ws::WsSinkOps;`, which pushes graph values out
//!   over the same connection.
//!
//! # Realtime only
//!
//! A live socket has no historical timeline to replay, so both factories
//! **reject [`RunMode::HistoricalFrom`] at wiring** (register B2) rather than
//! deadlocking the run. Record frames to a file or a `market` fixture and
//! replay *that* for a deterministic backtest.
//!
//! # The connection loop
//!
//! One task per source, spawned at graph `start()` (via
//! [`produce_async`](crate::async_source::produce_async), which defers to
//! `source_at_start`), running:
//!
//! 1. **Connect** to [`WsConfig::url`].
//! 2. **Send every [`WsConfig::subscriptions`] payload, in order** — on the
//!    first connection *and on every reconnection*. This is the step hand-rolled
//!    loops forget: a venue that drops you mid-session gives you a live socket
//!    with no subscriptions on it, and the graph then sits silent forever
//!    looking perfectly healthy.
//! 3. **Pump frames** until the socket closes, errors, or goes quiet for
//!    [`WsConfig::idle_timeout`]. `Ping` is answered with `Pong` explicitly (a
//!    `split()` socket does not auto-reply), and [`WsConfig::ping_interval`]
//!    sends our own keepalive.
//! 4. **Back off** ([`WsBackoff`], exponential with optional jitter) and go
//!    back to 1.
//!
//! A drop is **not** an error: it is a [`WsStatus`] transition and a
//! reconnect. The run aborts only when [`WsBackoff::max_attempts`] is set and
//! exhausted — the one way this source ever ends. With the default `None` it
//! retries forever, which is what a long-running trading process wants and
//! what a test must override.
//!
//! An attempt counts as successful once the peer sends its first frame, **not**
//! when the handshake completes. A venue that accepts the upgrade and then
//! closes — a rejected auth, a malformed subscription, an IP ban — would
//! otherwise reset the counter on every cycle, making `max_attempts`
//! unreachable and leaving the loop hammering the venue at
//! [`WsBackoff::initial`] forever.
//!
//! # Status is in-band
//!
//! [`WsStatus`] transitions are multiplexed onto the *same* channel as the
//! frames and split out on the graph, so `Connected` is strictly ordered
//! before the frames that followed it. A separate source could not promise
//! that. Transitions only — a reconnect storm ticks once per state change, not
//! once per cycle.
//!
//! # Credentials
//!
//! Venue URLs carry `?api_key=…` / `wss://user:pass@host` more often than not,
//! so **every** error site formats [`WsConfig::redacted`], never the raw URL.
//! [`redact_url`] masks userinfo and any query value whose key looks secret.
//!
//! # TLS
//!
//! `ws://` works with the `ws` feature alone. `wss://` needs `ws-tls`, which
//! adds rustls + the Mozilla root bundle and installs the **ring** provider on
//! first use.
//!
//! # Deviations
//!
//! There is no legacy `ws` adapter, so there is no parity oracle and nothing to
//! deviate *from*. Two departures from the `/new-adapter` conventions are worth
//! naming, because both are deliberate:
//!
//! 1. **[`ws_connect`] returns a [`WsConnection`] struct rather than a
//!    `*_with_status` tuple.** The convention (skill step 8a) assumes two
//!    outputs; this connection has three (frames, status, outbound sender) and
//!    a 3-tuple reads badly at every call site. What the convention is actually
//!    protecting — that the primary factory's signature stays unchanged — holds:
//!    [`ws_sub`] is untouched, and callers who want none of the extras pay
//!    nothing.
//! 2. **[`WsItem`] and [`WsItemOps`] are public despite being plumbing.** The
//!    `#[op(build = …)]` demux impls are public, so their input type cannot be
//!    private. Neither factory returns a [`WsItem`].
//!
//! One thing this adapter does **not** do, and deliberately: it never re-requests
//! anything venue-specific after a reconnect. A [`WsStatus::Connected`]
//! transition is the signal for *your* adapter to re-request a book snapshot —
//! the transport cannot know what a snapshot is. See
//! [`market`](crate::adapters::market) for the gap contract on the other side.
//!
//! ```no_run
//! use std::time::Duration;
//! use wingfoil::adapters::ws::{WsConfig, ws_sub};
//! use wingfoil::prelude::*;
//! use wingfoil::{RunFor, RunMode};
//!
//! # fn main() -> anyhow::Result<()> {
//! let g = GraphBuilder::new();
//! let cfg = WsConfig::new("wss://example.invalid/stream")
//!     .subscribe(r#"{"op":"subscribe","args":["trades.BTC-USD"]}"#)
//!     .idle_timeout(Duration::from_secs(30))
//!     .ping_interval(Duration::from_secs(15));
//!
//! let frames = ws_sub(&g, RunMode::RealTime, cfg)?;
//! frames.print().build().run(
//!     RunMode::RealTime,
//!     RunFor::Duration(Duration::from_secs(5)),
//! )?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use wingfoil::{NanoTime, RunMode};

use crate::Burst;
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::op::{Activation, Ctx, Op, Tick};
use wingfoil_derive::op;

/// Query-parameter keys whose *values* [`redact_url`] masks.
///
/// Matched case-insensitively as substrings, so `apiKey`, `X-MBX-APIKEY` and
/// `signature` are all covered by the stems below.
const SECRET_QUERY_KEYS: &[&str] = &["key", "token", "secret", "sign", "pass", "auth", "cred"];

/// What a mask replaces a secret with, in both userinfo and query values.
const REDACTED: &str = "***";

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// One WebSocket data frame, in either direction.
///
/// Control frames (`Ping`/`Pong`/`Close`) are handled by the connection loop
/// and never surface here — they are transport concerns, not payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsMessage {
    /// A UTF-8 text frame. Nearly every venue speaks JSON over this.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
}

impl WsMessage {
    /// The frame's bytes, whichever variant it is.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            WsMessage::Text(s) => s.as_bytes(),
            WsMessage::Binary(b) => b,
        }
    }

    /// The frame as text, or `None` if it was binary.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            WsMessage::Text(s) => Some(s),
            WsMessage::Binary(_) => None,
        }
    }
}

/// Only so the engine can hold a value slot before the first tick — an empty
/// text frame is not a meaningful message.
impl Default for WsMessage {
    fn default() -> Self {
        WsMessage::Text(String::new())
    }
}

impl From<String> for WsMessage {
    fn from(s: String) -> Self {
        WsMessage::Text(s)
    }
}

impl From<&str> for WsMessage {
    fn from(s: &str) -> Self {
        WsMessage::Text(s.to_owned())
    }
}

impl From<Vec<u8>> for WsMessage {
    fn from(b: Vec<u8>) -> Self {
        WsMessage::Binary(b)
    }
}

/// The connection's state, as the graph sees it.
///
/// Emitted on **transitions only**, in band with the frames, so a downstream
/// health gate ticks when something actually changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum WsStatus {
    /// Before the first successful connection.
    #[default]
    Disconnected,
    /// The socket is open and every configured subscription has been sent.
    Connected,
    /// The socket dropped; `attempt` is the 1-based retry about to be made
    /// after the backoff delay.
    Reconnecting {
        /// 1-based count of consecutive failed connections.
        attempt: u32,
    },
    /// [`WsBackoff::max_attempts`] was exhausted. Terminal — the run aborts
    /// immediately after this transition.
    Failed,
}

/// Exponential reconnect backoff.
///
/// `delay(attempt) = min(initial * multiplier^(attempt-1), max)`, optionally
/// with **equal jitter** — the actual sleep is drawn uniformly from
/// `[delay/2, delay]`. (Not *full* jitter, which would draw from `[0, delay]`
/// and can retry almost immediately.) Jitter is on by default and matters more
/// than it looks:
/// a venue restart disconnects every one of its clients at the same instant,
/// and an unjittered fleet then reconnects in lockstep forever.
#[derive(Clone, Copy, Debug)]
pub struct WsBackoff {
    /// Delay before the first retry.
    pub initial: Duration,
    /// Ceiling for the exponential growth.
    pub max: Duration,
    /// Growth factor per consecutive failure. Values below 1.0 are clamped to
    /// 1.0 at use — a shrinking backoff is always a config mistake.
    pub multiplier: f64,
    /// Draw the sleep from `[delay/2, delay]` instead of using `delay` exactly.
    pub jitter: bool,
    /// Give up after this many consecutive failures, aborting the run.
    /// `None` (the default) retries forever.
    pub max_attempts: Option<u32>,
}

impl Default for WsBackoff {
    fn default() -> Self {
        WsBackoff {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: true,
            max_attempts: None,
        }
    }
}

/// How to connect, what to subscribe to, and how to behave when it breaks.
///
/// Build from a URL and refine with the chainable setters:
///
/// ```
/// use std::time::Duration;
/// use wingfoil::adapters::ws::WsConfig;
///
/// let cfg = WsConfig::new("wss://stream.example.com/ws")
///     .subscribe(r#"{"op":"subscribe","args":["book.BTC-USD"]}"#)
///     .idle_timeout(Duration::from_secs(20));
/// ```
#[derive(Clone, Debug)]
pub struct WsConfig {
    /// `ws://` or `wss://` endpoint. `wss://` requires the `ws-tls` feature.
    pub url: String,
    /// Payloads sent, in order, immediately after **every** connect —
    /// including reconnects. This is what makes a reconnect actually restore
    /// the feed rather than leaving a silent socket.
    pub subscriptions: Vec<WsMessage>,
    /// Reconnect policy.
    pub backoff: WsBackoff,
    /// Treat the connection as dead if no frame arrives for this long. Venues
    /// commonly stop sending without closing the TCP connection, which no
    /// socket-level error will ever tell you about.
    pub idle_timeout: Option<Duration>,
    /// Send a `Ping` this often to keep intermediaries from reaping the
    /// connection.
    pub ping_interval: Option<Duration>,
    /// Bound on how far the producer may run ahead of the graph, in frames.
    /// `None` (the default) is unbounded, matching the other live `_sub`
    /// sources.
    pub buffer_size: Option<usize>,
}

impl WsConfig {
    /// A config for `url` with default backoff and no subscriptions.
    pub fn new(url: impl Into<String>) -> Self {
        WsConfig {
            url: url.into(),
            subscriptions: Vec::new(),
            backoff: WsBackoff::default(),
            idle_timeout: None,
            ping_interval: None,
            buffer_size: None,
        }
    }

    /// Append a payload to send after every connect.
    pub fn subscribe(mut self, message: impl Into<WsMessage>) -> Self {
        self.subscriptions.push(message.into());
        self
    }

    /// Replace the reconnect policy.
    pub fn backoff(mut self, backoff: WsBackoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Reconnect if no frame arrives for `timeout`.
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Send a keepalive `Ping` every `interval`.
    pub fn ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = Some(interval);
        self
    }

    /// Bound the producer to `frames` ahead of the graph.
    pub fn buffer_size(mut self, frames: usize) -> Self {
        self.buffer_size = Some(frames);
        self
    }

    /// The URL with credentials masked — **the only form that may reach an
    /// error message, a log or a graph abort.**
    pub fn redacted(&self) -> String {
        redact_url(&self.url)
    }
}

impl From<&str> for WsConfig {
    fn from(url: &str) -> Self {
        WsConfig::new(url)
    }
}

impl From<String> for WsConfig {
    fn from(url: String) -> Self {
        WsConfig::new(url)
    }
}

/// Mask credentials in a WebSocket URL: userinfo and secret-looking query
/// values.
///
/// Deliberately string-level rather than URL-parsing — it must never fail or
/// change the URL's shape, because its only job is to make an error message
/// safe to print. A string with no recognisable `scheme://` still has its query
/// masked; only the userinfo step is skipped, since there is no authority to
/// find one in.
///
/// Masking is **best effort**, not a guarantee: query values are matched
/// against the [`SECRET_QUERY_KEYS`] stems, so a venue that names its signature
/// parameter something short and unguessable (`?s=…`) will not be caught. Treat
/// it as a defence against the common shapes, not a licence to log URLs freely.
pub fn redact_url(url: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };

    // Userinfo lives between "scheme://" and the next '@' *before* any '/'.
    let masked_base = match base.split_once("://") {
        Some((scheme, rest)) => {
            let authority_end = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(authority_end);
            match authority.rsplit_once('@') {
                Some((userinfo, host)) => {
                    let user = userinfo.split_once(':').map_or(userinfo, |(u, _)| u);
                    format!("{scheme}://{user}:{REDACTED}@{host}{path}")
                }
                None => base.to_owned(),
            }
        }
        None => base.to_owned(),
    };

    let Some(query) = query else {
        return masked_base;
    };

    let masked_query = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if is_secret_key(k) => format!("{k}={REDACTED}"),
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&");

    format!("{masked_base}?{masked_query}")
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_QUERY_KEYS.iter().any(|stem| key.contains(stem))
}

/// The backoff sleep before retry number `attempt` (1-based).
///
/// `seed` supplies the jitter draw; the caller passes a wall-clock read, which
/// keeps this a pure function and therefore testable.
fn backoff_delay(backoff: &WsBackoff, attempt: u32, seed: u64) -> Duration {
    let multiplier = backoff.multiplier.max(1.0);
    let exponent = attempt.saturating_sub(1);
    // Saturating: `powi` overflows to +inf long before a real backoff would,
    // and `Duration::from_secs_f64` panics on a non-finite value.
    let grown = backoff.initial.as_secs_f64() * multiplier.powi(exponent.min(64) as i32);
    let capped = grown.min(backoff.max.as_secs_f64());
    let delay = if capped.is_finite() && capped > 0.0 {
        Duration::from_secs_f64(capped)
    } else {
        backoff.max
    };

    if !backoff.jitter {
        return delay;
    }
    // Equal jitter over [delay/2, delay]. A xorshift of the seed is plenty for
    // spreading a reconnect storm and costs no dependency.
    let mut x = seed | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let fraction = (x % 1_000_000) as f64 / 1_000_000.0;
    delay.mul_f64(0.5 + fraction / 2.0)
}

/// A handle for sending frames out over a live [`ws_connect`] connection.
///
/// Cheap to clone and safe to call from any thread, including a graph cycle:
/// the send is a non-blocking push onto an unbounded queue, never a socket
/// write and never a lock.
///
/// Frames sent while the socket is down are **queued, not dropped**, and go out
/// after the next connect completes its subscriptions. Anything that must
/// survive a reconnect belongs in [`WsConfig::subscriptions`] instead — the
/// queue replays a message once, not on every reconnect.
#[derive(Clone, Debug)]
pub struct WsSender {
    tx: mpsc::UnboundedSender<WsMessage>,
}

impl WsSender {
    /// Queue `message` for the connection.
    ///
    /// # Errors
    ///
    /// If the connection task has ended (the run finished, or the backoff gave
    /// up), so the frame can never be delivered.
    pub fn send(&self, message: impl Into<WsMessage>) -> Result<()> {
        self.tx
            .send(message.into())
            .map_err(|_| anyhow::anyhow!("ws: send on a closed connection"))
    }
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Frames and status, multiplexed onto one channel so their relative order
/// survives the trip to the graph.
///
/// Public only because the demultiplexing ops are — [`ws_sub`] and
/// [`ws_connect`] split this apart for you, and neither returns it. Reach for
/// it only if you are wiring the raw source yourself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsItem {
    /// A data frame from the venue.
    Message(WsMessage),
    /// A connection state transition.
    Status(WsStatus),
}

impl Default for WsItem {
    fn default() -> Self {
        WsItem::Status(WsStatus::Disconnected)
    }
}

/// Everything [`ws_connect`] gives you for one connection.
pub struct WsConnection {
    /// The data frames, grouped into a [`Burst`] per graph cycle.
    pub messages: Stream<Burst<WsMessage>>,
    /// Connection state, ticking only on transitions.
    pub status: Stream<WsStatus>,
    /// Outbound half — see [`WsSinkOps::ws_send`] to drive it from the graph.
    pub sender: WsSender,
}

/// Stream frames from a WebSocket endpoint, reconnecting as needed.
///
/// The common case: no status stream, no outbound frames. Use [`ws_connect`]
/// when you need either.
///
/// # Errors
///
/// At wiring time if `run_mode` is [`RunMode::HistoricalFrom`] (a live socket
/// cannot be replayed), or if the graph's tokio runtime cannot be created.
/// Connection failures are *not* wiring errors — they are retried per
/// [`WsBackoff`], and abort the run only once `max_attempts` is exhausted.
pub fn ws_sub(
    g: &GraphBuilder,
    run_mode: RunMode,
    config: impl Into<WsConfig>,
) -> Result<Stream<Burst<WsMessage>>> {
    let (items, _sender) = ws_items(g, run_mode, config.into())?;
    Ok(items.ws_messages())
}

/// Stream frames, connection status, and an outbound sender for one
/// WebSocket connection.
///
/// The status stream is fed in band with the frames, so a `Connected`
/// transition is ordered strictly before the frames that followed it.
///
/// # Errors
///
/// As [`ws_sub`].
pub fn ws_connect(
    g: &GraphBuilder,
    run_mode: RunMode,
    config: impl Into<WsConfig>,
) -> Result<WsConnection> {
    let (items, sender) = ws_items(g, run_mode, config.into())?;
    Ok(WsConnection {
        messages: items.ws_messages(),
        status: items.ws_status(),
        sender,
    })
}

/// The multiplexed source both factories are built on.
fn ws_items(
    g: &GraphBuilder,
    run_mode: RunMode,
    config: WsConfig,
) -> Result<(Stream<Burst<WsItem>>, WsSender)> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "ws: RunMode::HistoricalFrom is unsupported — a WebSocket is a \
             live, unbounded, wall-clock-stamped stream with no historical \
             timeline to replay; run it under RunMode::RealTime, or record its \
             frames and replay the recording"
        );
    }
    if config.url.starts_with("wss://") && !cfg!(feature = "ws-tls") {
        anyhow::bail!(
            "ws: {} needs TLS — enable the `ws-tls` feature (wss:// is not \
             supported by `ws` alone)",
            config.redacted()
        );
    }
    if !config.url.starts_with("ws://") && !config.url.starts_with("wss://") {
        anyhow::bail!(
            "ws: {} is not a WebSocket URL — expected a ws:// or wss:// scheme",
            config.redacted()
        );
    }

    let buffer_size = config.buffer_size;
    let (tx, rx) = mpsc::unbounded_channel();
    let stream = produce_async(
        g,
        move |_p: RunParams| async move { Ok(connection_stream(config, rx)) },
        buffer_size,
    )?;
    Ok((stream, WsSender { tx }))
}

/// The connect → subscribe → pump → back off loop, as an async stream of
/// timestamped [`WsItem`]s.
///
/// It ends only by yielding an error (backoff exhausted); every other outcome
/// is a reconnect.
fn connection_stream(
    config: WsConfig,
    mut outbound: mpsc::UnboundedReceiver<WsMessage>,
) -> impl futures::Stream<Item = Result<(NanoTime, WsItem)>> + Send {
    async_stream::stream! {
        install_crypto_provider();
        let mut attempt: u32 = 0;
        // Goes false once every `WsSender` has been dropped — which for
        // `ws_sub` is immediately, since it never hands one out. The select
        // below must then *stop polling* the outbound branch: a closed
        // `recv()` is instantly ready and would otherwise win the race against
        // the socket read on every turn, starving the connection of frames.
        let mut outbound_open = true;
        // The most recent connect failure, so the give-up message can say why.
        // Deliberately uninitialised: both arms of the connect below assign it
        // every time round the loop — `Some(reason)` on failure, `None` on a
        // success, which is what makes "the socket opened each time and then
        // closed" distinguishable from a connect that never landed.
        let mut last_error: Option<String>;

        loop {
            // A failed connect is a retry, not an error, so it is not yielded
            // per attempt — a venue that is down would produce one every
            // backoff interval. But the *reason* is kept: without it the
            // give-up message says only that N attempts failed, which is
            // nearly useless for the thing it is most needed for (telling a
            // DNS failure from a refused port from a TLS handshake error).
            match tokio_tungstenite::connect_async(&config.url).await {
                Err(e) => last_error = Some(e.to_string()),
                Ok((socket, _response)) => {
                // Note what is *not* here: `attempt = 0`. A completed handshake
                // is not yet evidence of a usable connection — see the reset
                // inside the frame branch below.
                last_error = None;
                let (mut write, mut read) = socket.split();

                // Subscribe before announcing Connected, so a downstream
                // that reacts to Connected cannot observe a socket that is
                // open but not yet subscribed.
                let mut subscribed = true;
                for payload in &config.subscriptions {
                    if write.send(to_tungstenite(payload.clone())).await.is_err() {
                        // A socket that dies mid-subscribe is treated exactly
                        // like a failed connect: fall through to the backoff
                        // and try the whole sequence again.
                        subscribed = false;
                        break;
                    }
                }

                if subscribed {
                    yield Ok((NanoTime::now(), WsItem::Status(WsStatus::Connected)));

                    let mut ping = config.ping_interval.map(|d| {
                        let mut interval = tokio::time::interval(d);
                        interval.set_missed_tick_behavior(
                            tokio::time::MissedTickBehavior::Delay,
                        );
                        // The first tick completes immediately; skip it so
                        // we don't ping the instant we connect.
                        interval.reset();
                        interval
                    });
                    let mut idle_deadline =
                        config.idle_timeout.map(|d| tokio::time::Instant::now() + d);

                    loop {
                        tokio::select! {
                            frame = read.next() => {
                                if let Some(d) = config.idle_timeout {
                                    idle_deadline = Some(tokio::time::Instant::now() + d);
                                }
                                // The peer is carrying the session rather than
                                // ending it, so this connection is real — only
                                // now is the retry counter cleared. Resetting
                                // it when `connect_async` returns `Ok` instead
                                // makes `max_attempts` unreachable for a venue
                                // that accepts the upgrade and then closes: a
                                // rejected auth, a bad subscription or an IP
                                // ban all look exactly like that, and each
                                // would spin the loop at `initial` forever.
                                //
                                // `Close` is deliberately *not* in this set. It
                                // is the one frame that means the session is
                                // over, so counting it would reinstate the bug
                                // for any venue that closes politely — which is
                                // most of them.
                                if matches!(
                                    frame,
                                    Some(Ok(TungsteniteMessage::Text(_)
                                        | TungsteniteMessage::Binary(_)
                                        | TungsteniteMessage::Ping(_)
                                        | TungsteniteMessage::Pong(_)))
                                ) {
                                    attempt = 0;
                                }
                                match frame {
                                    Some(Ok(TungsteniteMessage::Text(text))) => {
                                        yield Ok((
                                            NanoTime::now(),
                                            WsItem::Message(WsMessage::Text(text)),
                                        ));
                                    }
                                    Some(Ok(TungsteniteMessage::Binary(bytes))) => {
                                        yield Ok((
                                            NanoTime::now(),
                                            WsItem::Message(WsMessage::Binary(bytes)),
                                        ));
                                    }
                                    Some(Ok(TungsteniteMessage::Ping(payload))) => {
                                        // A split() socket does not answer
                                        // pings for us.
                                        if write
                                            .send(TungsteniteMessage::Pong(payload))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    // Pong/Frame carry no payload we expose;
                                    // receiving one still refreshed the idle
                                    // deadline above, which is their purpose.
                                    Some(Ok(_)) => {}
                                    // Keep the reason a live session died, for
                                    // the same purpose the connect error is
                                    // kept: it is what the give-up message has
                                    // to explain.
                                    Some(Err(e)) => {
                                        last_error = Some(e.to_string());
                                        break;
                                    }
                                    None => break,
                                }
                            }
                            outgoing = outbound.recv(), if outbound_open => {
                                match outgoing {
                                    Some(msg) => {
                                        if write.send(to_tungstenite(msg)).await.is_err() {
                                            break;
                                        }
                                    }
                                    // Every WsSender was dropped. The
                                    // connection stays up — a caller that
                                    // never sends is the normal case — but
                                    // this branch is done for good.
                                    None => outbound_open = false,
                                }
                            }
                            _ = async {
                                match ping.as_mut() {
                                    Some(interval) => {
                                        interval.tick().await;
                                    }
                                    None => std::future::pending().await,
                                }
                            } => {
                                if write
                                    .send(TungsteniteMessage::Ping(Vec::new()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            _ = async {
                                match idle_deadline {
                                    Some(deadline) => {
                                        tokio::time::sleep_until(deadline).await;
                                    }
                                    None => std::future::pending().await,
                                }
                            } => break,
                        }
                    }
                }
                }
            }

            attempt = attempt.saturating_add(1);
            if let Some(max) = config.backoff.max_attempts
                && attempt > max
            {
                yield Ok((NanoTime::now(), WsItem::Status(WsStatus::Failed)));
                yield Err(match &last_error {
                    Some(reason) => anyhow::anyhow!(
                        "ws: giving up on {} after {} consecutive failed \
                         connections; last error: {}",
                        config.redacted(),
                        max,
                        reason
                    ),
                    // No connect error recorded means the socket kept opening
                    // and then dying — a very different fault, worth saying so.
                    None => anyhow::anyhow!(
                        "ws: giving up on {} after {} consecutive failed \
                         connections; the socket opened each time and then closed",
                        config.redacted(),
                        max
                    ),
                });
                return;
            }

            yield Ok((
                NanoTime::now(),
                WsItem::Status(WsStatus::Reconnecting { attempt }),
            ));
            let delay = backoff_delay(&config.backoff, attempt, u64::from(NanoTime::now()));
            tokio::time::sleep(delay).await;
        }
    }
}

fn to_tungstenite(message: WsMessage) -> TungsteniteMessage {
    match message {
        WsMessage::Text(s) => TungsteniteMessage::Text(s),
        WsMessage::Binary(b) => TungsteniteMessage::Binary(b),
    }
}

/// rustls 0.23 refuses to pick a crypto provider when a process links more
/// than one, and panics at connect time. Installing ring explicitly (and
/// ignoring the error when something already installed one) keeps `wss://`
/// working in a binary that also pulls in `fix` or `web-tls`.
#[cfg(feature = "ws-tls")]
fn install_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(not(feature = "ws-tls"))]
fn install_crypto_provider() {}

// ---------------------------------------------------------------------------
// Demultiplexing ops
// ---------------------------------------------------------------------------

/// Selects the data frames out of a burst of multiplexed items, preserving the
/// burst.
pub struct WsMessagesOp;

#[op(build = ws_messages)]
impl Op for WsMessagesOp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a Burst<WsItem>,);
    type Out = Burst<WsMessage>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&Burst<WsItem>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<WsMessage>>> {
        let mut out = Burst::new();
        for item in input.0.iter() {
            if let WsItem::Message(m) = item {
                out.push(m.clone());
            }
        }
        Ok(if out.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(out)
        })
    }
}

/// Selects the status transitions out of a burst of multiplexed items.
///
/// Unlike the frames, status does **not** stay a burst: it is level-triggered
/// state, so the last transition in a cycle is the current one and the
/// intermediate ones are already history by the time the graph sees them.
pub struct WsStatusOp;

#[op(build = ws_status)]
impl Op for WsStatusOp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a Burst<WsItem>,);
    type Out = WsStatus;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&Burst<WsItem>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<WsStatus>> {
        let latest = input.0.iter().rev().find_map(|item| match item {
            WsItem::Status(s) => Some(*s),
            WsItem::Message(_) => None,
        });
        Ok(match latest {
            Some(s) => Tick::Value(s),
            None => Tick::Quiet,
        })
    }
}

/// Splitting a multiplexed [`WsItem`] stream into its two halves.
///
/// [`ws_sub`] and [`ws_connect`] apply this for you; it is public for the same
/// reason [`WsItem`] is.
pub trait WsItemOps {
    /// Just the data frames, burst preserved.
    fn ws_messages(&self) -> Stream<Burst<WsMessage>>;
    /// Just the connection state, latest-per-cycle.
    fn ws_status(&self) -> Stream<WsStatus>;
}

impl WsItemOps for Stream<Burst<WsItem>> {
    fn ws_messages(&self) -> Stream<Burst<WsMessage>> {
        self.wire(|b, h| b.ws_messages(h))
    }

    fn ws_status(&self) -> Stream<WsStatus> {
        self.wire(|b, h| b.ws_status(h))
    }
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// Sending graph values out over a [`ws_connect`] connection.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::ws::WsSinkOps;`.
pub trait WsSinkOps {
    /// Queue every value that ticks for delivery on `sender`'s connection.
    ///
    /// The cycle only pushes onto an unbounded queue — the socket write happens
    /// on the connection task — so this never blocks the engine on I/O. It
    /// aborts the run if the connection task has already ended, which for a
    /// live-only adapter means the run is finishing anyway.
    fn ws_send(&self, sender: &WsSender) -> Stream<()>;
}

impl WsSinkOps for Stream<Burst<WsMessage>> {
    fn ws_send(&self, sender: &WsSender) -> Stream<()> {
        let sender = sender.clone();
        self.for_each(move |burst: &Burst<WsMessage>| {
            for message in burst.iter() {
                sender
                    .send(message.clone())
                    .context("ws_send: queueing an outbound frame")?;
            }
            Ok(())
        })
    }
}

impl WsSinkOps for Stream<WsMessage> {
    fn ws_send(&self, sender: &WsSender) -> Stream<()> {
        let sender = sender.clone();
        self.for_each(move |message: &WsMessage| {
            sender
                .send(message.clone())
                .context("ws_send: queueing an outbound frame")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_userinfo() {
        assert_eq!(
            redact_url("wss://alice:hunter2@stream.example.com/ws"),
            "wss://alice:***@stream.example.com/ws"
        );
    }

    #[test]
    fn redacts_secret_query_values_only() {
        let redacted = redact_url(
            "wss://s.example.com/ws?symbol=BTC-USD&api_key=abc123&signature=deadbeef&depth=20",
        );
        assert_eq!(
            redacted,
            "wss://s.example.com/ws?symbol=BTC-USD&api_key=***&signature=***&depth=20"
        );
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("deadbeef"));
    }

    #[test]
    fn redaction_is_case_insensitive() {
        assert_eq!(
            redact_url("wss://s.example.com/ws?X-MBX-APIKEY=secretvalue"),
            "wss://s.example.com/ws?X-MBX-APIKEY=***"
        );
    }

    #[test]
    fn redaction_leaves_a_plain_url_alone() {
        let plain = "wss://stream.example.com/ws";
        assert_eq!(redact_url(plain), plain);
    }

    /// The `@` in a *path* must not be mistaken for userinfo — a real venue
    /// path can contain one, and mangling the URL in an error message would
    /// send someone debugging the wrong endpoint.
    #[test]
    fn redaction_ignores_an_at_sign_in_the_path() {
        let url = "wss://stream.example.com/ws/v1@latest";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn config_redacts_through_to_the_url() {
        let cfg = WsConfig::new("wss://k:s@example.com/ws?token=xyz");
        assert_eq!(cfg.redacted(), "wss://k:***@example.com/ws?token=***");
        assert!(!cfg.redacted().contains("xyz"));
    }

    fn fixed(initial_ms: u64, max_ms: u64, max_attempts: Option<u32>) -> WsBackoff {
        WsBackoff {
            initial: Duration::from_millis(initial_ms),
            max: Duration::from_millis(max_ms),
            multiplier: 2.0,
            jitter: false,
            max_attempts,
        }
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let b = fixed(100, 800, None);
        assert_eq!(backoff_delay(&b, 1, 0), Duration::from_millis(100));
        assert_eq!(backoff_delay(&b, 2, 0), Duration::from_millis(200));
        assert_eq!(backoff_delay(&b, 3, 0), Duration::from_millis(400));
        assert_eq!(backoff_delay(&b, 4, 0), Duration::from_millis(800));
        // Capped, not grown further.
        assert_eq!(backoff_delay(&b, 5, 0), Duration::from_millis(800));
        assert_eq!(backoff_delay(&b, 50, 0), Duration::from_millis(800));
    }

    /// A huge attempt count must stay finite: `powi` reaches `inf` quickly and
    /// `Duration::from_secs_f64(inf)` panics.
    #[test]
    fn backoff_saturates_instead_of_overflowing() {
        let b = fixed(100, 800, None);
        assert_eq!(backoff_delay(&b, u32::MAX, 0), Duration::from_millis(800));
    }

    #[test]
    fn jitter_stays_within_half_the_delay() {
        let b = WsBackoff {
            jitter: true,
            ..fixed(100, 800, None)
        };
        for seed in 0..1_000u64 {
            let d = backoff_delay(&b, 3, seed);
            assert!(
                d >= Duration::from_millis(200) && d <= Duration::from_millis(400),
                "seed {seed} gave {d:?}, outside [200ms, 400ms]"
            );
        }
    }

    #[test]
    fn jitter_actually_varies() {
        let b = WsBackoff {
            jitter: true,
            ..fixed(100, 800, None)
        };
        let first = backoff_delay(&b, 3, 1);
        assert!(
            (2..500).any(|seed| backoff_delay(&b, 3, seed) != first),
            "jitter produced a constant delay"
        );
    }

    /// A multiplier below 1.0 would otherwise *shrink* the delay on every
    /// failure — the opposite of a backoff.
    #[test]
    fn backoff_multiplier_below_one_is_clamped() {
        let b = WsBackoff {
            multiplier: 0.5,
            ..fixed(100, 800, None)
        };
        assert_eq!(backoff_delay(&b, 5, 0), Duration::from_millis(100));
    }

    #[test]
    fn status_default_is_disconnected() {
        assert_eq!(WsStatus::default(), WsStatus::Disconnected);
    }

    #[test]
    fn message_accessors() {
        assert_eq!(WsMessage::from("hi").as_text(), Some("hi"));
        assert_eq!(WsMessage::from(vec![1u8, 2]).as_text(), None);
        assert_eq!(WsMessage::from("hi").as_bytes(), b"hi");
        assert_eq!(WsMessage::from(vec![1u8, 2]).as_bytes(), &[1, 2]);
    }
}
