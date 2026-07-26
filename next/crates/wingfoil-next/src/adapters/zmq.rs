//! zmq adapter — real-time pub/sub messaging over ØMQ (ZeroMQ) sockets, with
//! optional service discovery. It ports the classic `wingfoil::adapters::zmq`
//! module onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `zmq` feature. Bring in what you need explicitly:
//!
//! - **Source** — the free function [`zmq_sub`] on a [`GraphBuilder`]: connects a
//!   `SUB` socket to a publisher and emits a `(data, status)` pair — a
//!   `Stream<Burst<T>>` of received messages plus a `Stream<ZmqStatus>` of
//!   connection-status transitions.
//! - **Sink** — the [`ZeroMqPub`] extension trait on any `Stream<T>`, enabled
//!   with `use wingfoil_next::adapters::zmq::ZeroMqPub;`: binds a `PUB` socket
//!   and publishes each value.
//!
//! # Source semantics (background thread over the channel)
//!
//! ZeroMQ sockets are synchronous and poll-based, so the subscriber dedicates a
//! background OS thread that polls the socket and feeds a
//! [`channel`](crate::fluent::SourceOps::channel) — the sync-streaming-client
//! shape from the adapter skill, rather than `produce_async`. Deliberately, the
//! `zmq` feature does **not** pull in `async`/tokio (matching the classic
//! adapter's design decision).
//!
//! Values arriving between graph cycles group into one [`Burst`]. A ZMQ *monitor*
//! socket is opened alongside the data socket so `Connected` / `Disconnected`
//! transitions surface on the status stream without external state; the subscriber
//! multiplexes data and status over the one channel (an internal `ZmqEvent`
//! envelope), then splits them back into the two streams.
//!
//! The subscriber is a live, never-closing source: it **rejects
//! [`RunMode::HistoricalFrom`] at wiring time** and returns an error. It cannot
//! replay historically — next's channel receiver block-collects the whole stream
//! up front in a historical run, so an unbounded live producer would deadlock the
//! graph at `start`. Run [`zmq_sub`] under [`RunMode::RealTime`].
//!
//! # Sink
//!
//! [`ZeroMqPub::zmq_pub`] binds a `PUB` socket (on `127.0.0.1` by default, or a
//! routable address via [`zmq_pub_on`](ZeroMqPub::zmq_pub_on)) and publishes the
//! stream's current value each cycle. Because a fresh `SUB` peer misses messages
//! sent before its subscription filter propagates (ZeroMQ's *slow-joiner*
//! problem), the publisher watches its own monitor socket for the first accepted
//! connection and **buffers** outgoing messages until the subscriber is ready
//! (up to [`BUFFER_TIMEOUT`], plus a short subscription-propagation delay). The
//! publisher is realtime-only: a [`RunMode::HistoricalFrom`] run **aborts** with a
//! "real-time" error on the first cycle (publishing fast-forwarded historical
//! data to a live socket is meaningless — matching classic, which errors rather
//! than the skill's no-op default for exporters).
//!
//! # Service discovery (pluggable backend)
//!
//! Both endpoints accept a bare address *or* a `(name, registry)` pair for
//! discovery-based lookup, via `impl Into<_>` config wrappers ([`ZmqSubConfig`] /
//! [`ZmqPubRegistration`]) with `From` impls — one signature, several call
//! shapes. The registry is a pluggable backend behind the [`ZmqRegistry`] trait;
//! with the `etcd` feature also enabled, [`EtcdRegistry`] becomes a discovery
//! backend that stores the publisher address in etcd under a lease.
//!
//! ```ignore
//! stream.zmq_pub(5556, ());                                   // no registration
//! stream.zmq_pub(5556, ("quotes", EtcdRegistry::new(conn)));  // register in etcd
//!
//! zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, "tcp://host:5556")?;               // direct
//! zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, ("quotes", EtcdRegistry::new(conn)))?; // discovery
//! ```
//!
//! # Deviations from classic
//!
//! Every classic *capability* (sub with a status stream, pub with slow-joiner
//! buffering, the `ZmqRegistry`/`EtcdRegistry` discovery backend, `zmq_pub_on`
//! for routable binds) is preserved. The surface differs in three deliberate
//! ways:
//!
//! 1. **[`zmq_sub`] takes a [`GraphBuilder`] and a [`RunMode`].** It wires a
//!    `channel` source on the builder (like every next source) and needs the run
//!    mode to reject `HistoricalFrom` at wiring time — next's channel is bimodal
//!    and would deadlock rather than erroring at run start the way classic's
//!    realtime-only `ReceiverStream` does.
//! 2. **[`ZeroMqPub::zmq_pub`] returns `Stream<()>`** (a sink to add to the
//!    graph) instead of classic's `Rc<dyn Node>`. Binding, registration and the
//!    run-mode check happen lazily on the first cycle (like classic's `start`),
//!    so a historical run still errors with "real-time" *before* touching the
//!    registry.
//! 3. **The wire envelope is next-local.** Messages are `bincode`-framed with an
//!    internal envelope, so a next publisher interoperates with a next
//!    subscriber but is **not** wire-compatible with a classic/Python
//!    `wingfoil` peer — cross-language interop lands with the Python bindings
//!    (port-plan Phase 6).
//!
//! Two smaller surface reductions: the internal `ZmqEvent<T>` data/status
//! multiplexing envelope is **private** here (classic exposed it as `pub`, but it
//! is purely an internal transport detail); and `ZmqStatus` derives `Eq` in
//! addition to classic's `PartialEq` (harmless).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wingfoil::RunMode;

use crate::Burst;
use crate::channel::ChannelSender;
use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::interp::StopHandle;
use crate::op::{Activation, Ctx, Tick};

mod registry;

use registry::ZmqSubResolution;
pub use registry::{ZmqHandle, ZmqPubRegistration, ZmqRegistry, ZmqSubConfig};

#[cfg(feature = "etcd")]
pub use registry::EtcdRegistry;

/// Distinct monitor `inproc://` addresses across every socket in the process.
static MONITOR_ID: AtomicUsize = AtomicUsize::new(0);

// ZeroMQ socket monitor event flags (from `zmq.h`).
const ZMQ_EVENT_CONNECTED: u16 = 0x0001;
const ZMQ_EVENT_ACCEPTED: u16 = 0x0008;
const ZMQ_EVENT_DISCONNECTED: u16 = 0x0200;

/// Maximum time the publisher buffers messages while waiting for a subscriber to
/// connect. Messages buffered longer than this are discarded — a subscriber
/// connecting after this window should not receive stale data.
pub const BUFFER_TIMEOUT: Duration = Duration::from_millis(500);

/// Connection status reported by the subscriber's socket monitor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ZmqStatus {
    /// The `SUB` socket has connected to a publisher.
    Connected,
    /// The `SUB` socket is not connected (initial state, or after a disconnect /
    /// publisher `EndOfStream`).
    #[default]
    Disconnected,
}

/// Internal per-message envelope multiplexed over the one channel: either a
/// decoded data value or a connection-status transition. The subscriber splits
/// these back into the `(data, status)` streams so a `Connected` transition
/// stays correctly ordered relative to the values around it.
#[derive(Debug, Clone)]
enum ZmqEvent<T> {
    Data(T),
    Status(ZmqStatus),
}

impl<T: Default> Default for ZmqEvent<T> {
    fn default() -> Self {
        ZmqEvent::Data(T::default())
    }
}

/// The `bincode`-framed wire envelope. Realtime-only, so a publisher only ever
/// sends [`WireMessage::Value`] and [`WireMessage::EndOfStream`]. Mirrors the
/// variants of classic wingfoil's `channel::Message` (including its `Error`
/// case) but is **not** byte-compatible with it (see the module-level
/// deviations).
#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireMessage<T> {
    /// A published value.
    Value(T),
    /// The publisher is shutting down cleanly.
    EndOfStream,
    /// An error, carried in the envelope for symmetry with classic. Not sent by
    /// this publisher; the subscriber synthesizes it locally on a decode failure
    /// (see [`run_subscriber`]) to route the error through the same match arm.
    Error(String),
}

/// Serialize the payload-free `EndOfStream` frame. The variant carries no `T`,
/// so any element type produces identical bytes — used at publisher teardown
/// without naming the stream's element type.
fn end_of_stream_bytes() -> Result<Vec<u8>> {
    bincode::serialize(&WireMessage::<()>::EndOfStream).context("zmq_pub: serializing EndOfStream")
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// Sets the shared stop flag when the graph is torn down, so the background
/// subscriber thread exits promptly on its next poll timeout instead of leaking.
struct ThreadStopGuard(Arc<AtomicBool>);

impl Drop for ThreadStopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Connect a `SUB` socket to a ZMQ publisher and stream incoming messages.
///
/// Accepts either a direct address (`"tcp://host:5556"`) or a `(name, registry)`
/// tuple for discovery-based lookup. With a registry tuple the address is
/// resolved **at call time** (not at graph start), so the publisher must have
/// registered before [`zmq_sub`] is called; otherwise the lookup errors here.
///
/// Returns a `(data, status)` pair:
/// - `data` ticks with each burst of received messages, decoded as `T`.
/// - `status` ticks on each connection-status transition
///   ([`ZmqStatus::Connected`] / [`ZmqStatus::Disconnected`]).
///
/// # Errors
///
/// Returns an error at **wiring time** if:
/// - `run_mode` is [`RunMode::HistoricalFrom`] — the subscriber is a live,
///   never-closing source with no historical timeline to replay, and next's
///   channel receiver would block-collect it up front and deadlock at `start`.
///   Run under [`RunMode::RealTime`].
/// - a `(name, registry)` config is passed and the registry lookup fails (e.g.
///   the backend is unreachable, or no publisher registered under `name`).
///
/// A socket connect failure surfaces **during the run** via the channel's error
/// path (the socket lives on the background thread), aborting the run with
/// context — matching classic, whose subscriber also connects on its thread.
pub fn zmq_sub<T>(
    g: &GraphBuilder,
    run_mode: RunMode,
    config: impl Into<ZmqSubConfig>,
) -> Result<(Stream<Burst<T>>, Stream<ZmqStatus>)>
where
    T: Clone + Default + Send + DeserializeOwned + 'static,
{
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "zmq_sub: RunMode::HistoricalFrom is unsupported — the subscriber is a \
             live, unbounded, never-closing source with no historical timeline to \
             replay; run zmq_sub under RunMode::RealTime"
        );
    }

    let address = match config.into().0 {
        ZmqSubResolution::Direct(addr) => addr,
        ZmqSubResolution::Discover(name, registry) => registry
            .lookup(&name)
            .with_context(|| format!("zmq_sub: resolving {name:?} via registry"))?,
    };

    // Defer the socket connect and subscriber thread to graph **start**: the
    // factory above did only pure wiring work (address parse / registry lookup /
    // historical rejection), touching no socket. `setup` runs at `start()`,
    // spawns the polling thread, and returns a [`StopHandle`] wrapping the
    // [`ThreadStopGuard`] so the thread is signalled to stop at teardown.
    let events = g.source_at_start::<ZmqEvent<T>, _>(move |sender| {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let address = address.clone();
        std::thread::Builder::new()
            .name("zmq-sub".into())
            .spawn(move || {
                if let Err(e) = run_subscriber::<T>(&address, &sender, &thread_stop) {
                    sender.send_error(e);
                }
            })
            .context("zmq_sub: spawning subscriber thread")?;
        Ok(StopHandle::new(ThreadStopGuard(stop)))
    });

    let data = events.map_filter(|burst: &Burst<ZmqEvent<T>>| {
        let data: Burst<T> = burst
            .iter()
            .filter_map(|e| match e {
                ZmqEvent::Data(v) => Some(v.clone()),
                ZmqEvent::Status(_) => None,
            })
            .collect();
        let ticked = !data.is_empty();
        (data, ticked)
    });

    let status = events.map_filter(|burst: &Burst<ZmqEvent<T>>| {
        match burst
            .iter()
            .filter_map(|e| match e {
                ZmqEvent::Status(s) => Some(s.clone()),
                ZmqEvent::Data(_) => None,
            })
            .last()
        {
            Some(s) => (s, true),
            None => (ZmqStatus::default(), false),
        }
    });

    Ok((data, status))
}

/// The background thread body: poll the data + monitor sockets and feed the
/// channel. Returns `Ok(())` on a clean stop (stop flag, receiver gone, or
/// publisher `EndOfStream`); an `Err` is forwarded to the graph as a channel
/// error by the caller.
fn run_subscriber<T>(
    address: &str,
    sender: &ChannelSender<ZmqEvent<T>>,
    stop: &AtomicBool,
) -> Result<()>
where
    T: Clone + Default + Send + DeserializeOwned + 'static,
{
    let context = zmq::Context::new();
    let socket = context
        .socket(zmq::SUB)
        .context("zmq_sub: creating SUB socket")?;
    socket
        .connect(address)
        .with_context(|| format!("zmq_sub: connecting to {address}"))?;
    socket
        .set_subscribe(b"")
        .context("zmq_sub: setting subscription filter")?;

    let monitor_id = MONITOR_ID.fetch_add(1, Ordering::Relaxed);
    let monitor_addr = format!("inproc://zmq-sub-monitor-{monitor_id}");
    socket
        .monitor(
            &monitor_addr,
            (ZMQ_EVENT_CONNECTED | ZMQ_EVENT_DISCONNECTED) as i32,
        )
        .context("zmq_sub: registering socket monitor")?;
    let monitor = context
        .socket(zmq::PAIR)
        .context("zmq_sub: creating monitor socket")?;
    monitor
        .connect(&monitor_addr)
        .context("zmq_sub: connecting monitor socket")?;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut items = [
            socket.as_poll_item(zmq::POLLIN),
            monitor.as_poll_item(zmq::POLLIN),
        ];
        zmq::poll(&mut items, 200).context("zmq_sub: polling sockets")?;

        if items[1].is_readable() {
            let event_frame = monitor
                .recv_msg(0)
                .context("zmq_sub: reading monitor event")?;
            // Drain the trailing address frame.
            while monitor.get_rcvmore().unwrap_or(false) {
                let _ = monitor.recv_msg(0);
            }
            if event_frame.len() >= 2 {
                let event_id = u16::from_le_bytes([event_frame[0], event_frame[1]]);
                let status = match event_id {
                    ZMQ_EVENT_CONNECTED => Some(ZmqStatus::Connected),
                    ZMQ_EVENT_DISCONNECTED => Some(ZmqStatus::Disconnected),
                    _ => None,
                };
                if let Some(status) = status
                    && !sender.send(ZmqEvent::Status(status))
                {
                    // Receiver gone — a normal teardown race.
                    return Ok(());
                }
            }
        }

        if items[0].is_readable() {
            let bytes = socket.recv_bytes(0).context("zmq_sub: receiving message")?;
            let msg: WireMessage<T> = bincode::deserialize(&bytes)
                .unwrap_or_else(|err| WireMessage::Error(format!("{err}")));
            match msg {
                WireMessage::Value(v) => {
                    if !sender.send(ZmqEvent::Data(v)) {
                        return Ok(());
                    }
                }
                WireMessage::EndOfStream => {
                    // Publisher is gone: report a final disconnect, then close.
                    let _ = sender.send(ZmqEvent::Status(ZmqStatus::Disconnected));
                    sender.close();
                    return Ok(());
                }
                WireMessage::Error(err) => {
                    sender.send_error(anyhow::anyhow!(err).context("zmq_sub: decoding message"));
                    return Ok(());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// Graph-thread-local publisher state. Bound and registered lazily on the first
/// cycle (like classic's `start`); its [`Drop`] sends `EndOfStream` and revokes
/// the registry handle at graph teardown (like classic's `stop`).
struct ZmqPubState {
    port: u16,
    bind_address: String,
    registration: ZmqPubRegistration,
    socket: Option<zmq::Socket>,
    monitor: Option<zmq::Socket>,
    registry_handle: Option<Box<dyn ZmqHandle>>,
    subscriber_connected: bool,
    accepted_at: Option<Instant>,
    buffer: Vec<Vec<u8>>,
    buffer_start: Option<Instant>,
    started: bool,
}

impl ZmqPubState {
    fn new(port: u16, bind_address: String, registration: ZmqPubRegistration) -> Self {
        Self {
            port,
            bind_address,
            registration,
            socket: None,
            monitor: None,
            registry_handle: None,
            subscriber_connected: false,
            accepted_at: None,
            buffer: Vec::new(),
            buffer_start: None,
            started: false,
        }
    }

    /// Bind the `PUB` socket, open its monitor, and register with a discovery
    /// backend. Runs once, on the first cycle.
    fn start(&mut self) -> Result<()> {
        let context = zmq::Context::new();
        let socket = context
            .socket(zmq::SocketType::PUB)
            .context("zmq_pub: creating PUB socket")?;

        let monitor_id = MONITOR_ID.fetch_add(1, Ordering::Relaxed);
        let monitor_addr = format!("inproc://zmq-pub-monitor-{monitor_id}");
        socket
            .monitor(&monitor_addr, ZMQ_EVENT_ACCEPTED as i32)
            .context("zmq_pub: registering socket monitor")?;
        let monitor = context
            .socket(zmq::PAIR)
            .context("zmq_pub: creating monitor socket")?;
        monitor
            .connect(&monitor_addr)
            .context("zmq_pub: connecting monitor socket")?;

        let address = format!("tcp://{}:{}", self.bind_address, self.port);
        socket
            .bind(&address)
            .with_context(|| format!("zmq_pub: binding {address}"))?;

        if let Some((name, registry)) = &self.registration.0 {
            self.registry_handle = Some(
                registry
                    .register(name, &address)
                    .with_context(|| format!("zmq_pub: registering {name:?}"))?,
            );
        }
        self.socket = Some(socket);
        self.monitor = Some(monitor);
        self.started = true;
        Ok(())
    }

    /// Drain any pending monitor events, noting when a subscriber's TCP
    /// connection is accepted.
    fn check_monitor(&mut self) {
        let Some(monitor) = self.monitor.as_ref() else {
            return;
        };
        loop {
            let mut items = [monitor.as_poll_item(zmq::POLLIN)];
            if zmq::poll(&mut items, 0).is_err() || !items[0].is_readable() {
                break;
            }
            let Ok(event_frame) = monitor.recv_msg(0) else {
                break;
            };
            while monitor.get_rcvmore().unwrap_or(false) {
                let _ = monitor.recv_msg(0);
            }
            if event_frame.len() >= 2 {
                let event_id = u16::from_le_bytes([event_frame[0], event_frame[1]]);
                if event_id == ZMQ_EVENT_ACCEPTED {
                    self.accepted_at = Some(Instant::now());
                }
            }
        }
    }

    /// Publish (or buffer) one already-serialized message.
    fn publish(&mut self, data: Vec<u8>) -> Result<()> {
        self.check_monitor();
        if !self.subscriber_connected
            && let Some(accepted_at) = self.accepted_at
        {
            // After the TCP accept, the subscriber still needs a moment for its
            // subscription filter to propagate through ZMQ internals. 50 ms is
            // generous for slow CI while keeping buffered-message latency to
            // roughly one graph cycle.
            if accepted_at.elapsed() >= Duration::from_millis(50) {
                self.subscriber_connected = true;
            }
        }

        let sock = self
            .socket
            .as_ref()
            .expect("invariant: socket bound on the first cycle");

        if self.subscriber_connected {
            for buffered in self.buffer.drain(..) {
                sock.send(buffered, 0).context("zmq_pub: flushing buffer")?;
            }
            self.buffer_start = None;
            sock.send(data, 0).context("zmq_pub: sending message")?;
        } else {
            // No subscriber yet — buffer the message. Only discard stale buffered
            // data while no subscriber has begun connecting; once a TCP accept is
            // observed we are committed to flushing to that subscriber within the
            // propagation window, so clearing here would drop the very messages the
            // buffer exists to preserve.
            let now = Instant::now();
            let start = *self.buffer_start.get_or_insert(now);
            if self.accepted_at.is_none() && now.duration_since(start) > BUFFER_TIMEOUT {
                self.buffer.clear();
                self.buffer_start = Some(now);
            }
            self.buffer.push(data);
        }
        Ok(())
    }
}

impl Drop for ZmqPubState {
    fn drop(&mut self) {
        if let Some(mut handle) = self.registry_handle.take() {
            handle.revoke();
        }
        // Best-effort EndOfStream so a live subscriber sees a clean disconnect.
        if let Some(sock) = self.socket.as_ref()
            && let Ok(bytes) = end_of_stream_bytes()
        {
            let _ = sock.send(bytes, 0);
        }
    }
}

/// Fluent API for publishing a stream over a ZMQ `PUB` socket — an extension
/// trait on any `Stream<T>` whose values are [`Serialize`].
///
/// `use`ing it enables `stream.zmq_pub(port, registration)` chaining. Pass `()`
/// for no registration, or `("name", registry)` to register the bound address
/// with a discovery backend.
pub trait ZeroMqPub<T> {
    /// Bind on `127.0.0.1:port` and publish each value. Returns the sink
    /// `Stream<()>` (add it to the graph). Optionally register the address with a
    /// discovery backend.
    ///
    /// **Multi-host note:** `127.0.0.1` is loopback and unreachable from other
    /// machines. When registering in a multi-host deployment, use
    /// [`zmq_pub_on`](Self::zmq_pub_on) with a routable bind address so the stored
    /// address is reachable by remote subscribers.
    ///
    /// The publisher is realtime-only: a [`RunMode::HistoricalFrom`] run aborts
    /// with a "real-time" error on the first cycle.
    fn zmq_pub(&self, port: u16, registration: impl Into<ZmqPubRegistration>) -> Stream<()>;

    /// Bind on `address:port` (rather than `127.0.0.1`) and publish each value.
    /// Use this in multi-host deployments where `127.0.0.1` is not reachable.
    fn zmq_pub_on(
        &self,
        address: &str,
        port: u16,
        registration: impl Into<ZmqPubRegistration>,
    ) -> Stream<()>;
}

impl<T> ZeroMqPub<T> for Stream<T>
where
    T: Serialize + Clone + Default + Send + 'static,
{
    fn zmq_pub(&self, port: u16, registration: impl Into<ZmqPubRegistration>) -> Stream<()> {
        self.zmq_pub_on("127.0.0.1", port, registration)
    }

    fn zmq_pub_on(
        &self,
        address: &str,
        port: u16,
        registration: impl Into<ZmqPubRegistration>,
    ) -> Stream<()> {
        let state = ZmqPubState::new(port, address.to_string(), registration.into());
        self.wire(move |b, h| {
            b.register_op1(
                h,
                "zmq_pub",
                Activation::NONE,
                state,
                || (),
                move |state: &mut ZmqPubState, _s: &mut (), value: &T, ctx: &mut Ctx<'_>| {
                    if !state.started {
                        if let RunMode::HistoricalFrom(_) = ctx.run_mode() {
                            // Reject historical before binding or touching the
                            // registry, so the error names the run mode — not a
                            // registry connection failure (classic ordering).
                            anyhow::bail!("ZMQ nodes only support real-time mode");
                        }
                        state.start()?;
                    }
                    let data = bincode::serialize(&WireMessage::Value(value.clone()))
                        .context("zmq_pub: serializing message")?;
                    state.publish(data)?;
                    Ok(Tick::Value(()))
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_config_from_address() {
        let direct = ZmqSubConfig::from("tcp://host:5556");
        assert!(matches!(direct.0, ZmqSubResolution::Direct(ref a) if a == "tcp://host:5556"));

        let owned = ZmqSubConfig::from(String::from("tcp://host:5557"));
        assert!(matches!(owned.0, ZmqSubResolution::Direct(ref a) if a == "tcp://host:5557"));
    }

    #[test]
    fn pub_registration_from_unit_is_none() {
        let reg = ZmqPubRegistration::from(());
        assert!(reg.0.is_none());
    }

    #[test]
    fn end_of_stream_frame_round_trips() {
        let bytes = end_of_stream_bytes().unwrap();
        let decoded: WireMessage<u64> = bincode::deserialize(&bytes).unwrap();
        assert!(matches!(decoded, WireMessage::EndOfStream));
    }
}
