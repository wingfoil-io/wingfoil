//! `web_pub` — stream values out to connected WebSocket clients.
//!
//! Each call to [`WebSinkOps::web_pub`] registers a topic on the [`WebServer`].
//! Upstream values are serialized with the server's codec, wrapped in an
//! [`Envelope`], and delivered to every WebSocket connection subscribed to that
//! topic.
//!
//! The payload shape follows the stream's shape: a scalar `Stream<T>` puts one
//! JSON/bincode value per frame on the wire (the browser client treats it as a
//! one-element burst), while
//! [`WebBurstSinkOps::web_pub_bursts`] on a `Stream<Burst<T>>` puts the whole
//! same-`time_ns` group on the wire as one **array** frame — atomic, so a lossy
//! drop can never split a timestamp. See `subscribe` / `subscribeBurst` in
//! `@wingfoil/client`.
//!
//! How a client that cannot keep up is handled is the server's
//! [`Delivery`](super::Delivery) policy, and it differs by run mode. In **real time** slow
//! consumers do **not** back-pressure the graph: each client has a bounded
//! outbound queue and the publisher drops rather than waits, so a frozen
//! browser tab simply loses frames. In a **historical replay** there is no live clock to
//! fall behind, so the default pace-to-the-slowest-subscriber policy makes the
//! publisher wait instead of corrupting the replay.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use axum::body::Bytes;
use serde::Serialize;
use wingfoil::NanoTime;

use super::codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope};
use super::server::{WebServer, WebServerInner};
use crate::Burst;
use crate::async_source::consume_async_bursts;
use crate::burst;
use crate::fluent::{Stream, StreamOps};
use crate::interp::StopHandle;

/// Depth of the sink channel between the graph thread and the publish consumer
/// task, in **instants** (one entry per burst, not per frame — a burst is
/// indivisible on the graph side).
///
/// This bound *is* the back-pressure: `spawn_sink`'s `send_blocking` parks the
/// graph thread when the channel is full, so a lossless consumer waiting on a
/// slow socket paces the graph without any separate pacing mechanism. Sized so
/// a lossless replay may run this far ahead of its slowest subscriber before it
/// waits, and so a lossy real-time graph — whose consumer never waits on a
/// client at all — effectively never reaches it.
const PUBLISH_INFLIGHT_INSTANTS: usize = 64;

/// Extension trait providing a fluent API for publishing a stream to browsers.
///
/// Implemented for any `Stream<T>` whose value serializes — one payload per
/// frame, which the browser client treats as a one-element burst. Bring it in
/// with `use wingfoil::adapters::web::WebSinkOps;`.
pub trait WebSinkOps {
    /// Publish every upstream value on `topic`. Returns the sink `Stream<()>`.
    ///
    /// Works identically under [`RunMode::RealTime`](wingfoil::RunMode::RealTime)
    /// and [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom) — a
    /// historical replay (or any finite `RunFor`) streams its values out to
    /// subscribed clients just like a live run, which is what powers
    /// browser-side visualisation of a backtest or slow computation. When the
    /// run ends, a [`ControlMessage::Complete`] is delivered on the topic so
    /// clients can tell "replay finished" from a transport drop.
    ///
    /// The exception is a server built with
    /// [`WebServerBuilder::start_historical`](super::WebServerBuilder::start_historical),
    /// whose [`WebServer::is_historical_noop`] flag makes both `web_pub` and
    /// [`web_sub`](super::web_sub) no-ops: the sink drains the stream without
    /// touching the network, so a backtest that does *not* want a server can run
    /// the same graph unmodified.
    ///
    /// Back-pressure note: which side waits is the server's
    /// [`Delivery`](super::Delivery) policy, and it differs by run mode. Under
    /// the default [`Delivery::Auto`](super::Delivery::Auto) a **real-time**
    /// client never stalls the graph — it has a bounded, lossy outbound path, so
    /// a client that cannot keep up drops frames — while a **historical** replay
    /// is delivered losslessly, the publisher pacing itself to the slowest
    /// subscribed client so the browser sees the whole replay in order. With no
    /// subscribers nothing is waited on either way.
    ///
    /// [`Delivery::Lossy`](super::Delivery::Lossy) forces the never-block
    /// behaviour in both modes; a client that stops reading without closing its
    /// socket will otherwise stall a lossless run.
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time only if the graph's tokio runtime cannot
    /// be created. A codec failure aborts the run with context; a publish with
    /// no subscribers is not an error (frames are simply dropped).
    fn web_pub(&self, server: &WebServer, topic: impl Into<String>) -> Result<Stream<()>>;
}

impl<T> WebSinkOps for Stream<T>
where
    T: Serialize + Clone + Default + Send + 'static,
{
    fn web_pub(&self, server: &WebServer, topic: impl Into<String>) -> Result<Stream<()>> {
        if server.is_historical_noop() {
            return Ok(noop_sink(self));
        }
        let framed = self
            .with_time()
            .map(|(time, value): &(NanoTime, T)| burst![(*time, value.clone())]);
        publish_frames(&framed, server, topic.into())
    }
}

/// Extension trait for publishing a **burst** stream as one atomic array frame
/// per instant.
///
/// A separate trait (and method name) from [`WebSinkOps`] because `Burst<T>` is
/// a `TinyVec`, which is not `Serialize` — so the burst form cannot simply be a
/// second impl of the same trait. Bring it in with
/// `use wingfoil::adapters::web::WebBurstSinkOps;`.
pub trait WebBurstSinkOps {
    /// Publish each burst on `topic` as one array frame carrying the whole
    /// same-`time_ns` group. Returns the sink `Stream<()>`.
    ///
    /// Atomic on the wire, so a lossy drop can never split a timestamp (and
    /// under a lossless [`Delivery`](super::Delivery) there is no drop to split
    /// it); the client surfaces the group whole via `subscribeBurst`. Semantics are
    /// otherwise identical to [`WebSinkOps::web_pub`] — including the
    /// end-of-run [`ControlMessage::Complete`] and the historical-no-op server.
    ///
    /// Legacy had no burst overload: callers mapped `Burst<T>` to `Vec<T>`
    /// themselves and used `web_pub`. This does that conversion internally and
    /// produces byte-identical frames.
    ///
    /// # Errors
    ///
    /// As [`WebSinkOps::web_pub`].
    fn web_pub_bursts(&self, server: &WebServer, topic: impl Into<String>) -> Result<Stream<()>>;

    /// Publish **each value** of the burst as its own frame, byte-identical to
    /// what [`WebSinkOps::web_pub`] would have produced for that value.
    ///
    /// The counterpart to [`web_pub_bursts`](Self::web_pub_bursts), which sends
    /// the whole group as one array frame: this one keeps the scalar wire
    /// format, so a client written against `web_pub` needs no change. Use it
    /// when the pipeline is burst-shaped only so that nothing gets dropped —
    /// the alternative, `collapse()` before `web_pub`, keeps just the burst's
    /// last value and silently discards the rest, which on an order or fill
    /// topic is data loss that only shows up under load.
    ///
    /// Frames within one burst go out in burst order and all carry the same
    /// `time_ns`. Unlike `web_pub_bursts` they are not atomic on the wire, so a
    /// lossy client drop can split a group; take that trade only when the client
    /// cannot be changed. (Under a lossless
    /// [`Delivery`](super::Delivery) nothing is dropped, so the distinction
    /// falls away — but the wire format is the same either way, and the run mode
    /// is not the sink's to assume.)
    ///
    /// # Why this is not just `web_pub` on a burst stream
    ///
    /// Elsewhere in the tree, burst support is a second impl of the *same*
    /// trait under the *same* method name, dispatched on the receiver's shape —
    /// `otlp_spans` and `latency_report` both do that, and it is the shape to
    /// prefer, since a suffix is a cost paid by every caller.
    ///
    /// It is not available here. [`WebSinkOps`] is not generic over the payload
    /// type, so `impl WebSinkOps for Stream<T>` and
    /// `impl WebSinkOps for Stream<Burst<T>>` unify at `T = Burst<U>` and
    /// collide on coherence — the `T: Serialize` bound does not separate them,
    /// because overlap checking does not consider where-clauses. Hence the
    /// separate trait (which is also why `web_pub_bursts` already lived here),
    /// and hence a distinct method name.
    ///
    /// The name follows the tree-wide suffix convention this trait set:
    /// `_each` means *one frame (or stamp, or span) per value in the burst* —
    /// here and on
    /// [`stamp_each`](crate::latency::LatencyBurstStreamOps::stamp_each) —
    /// while `_bursts` means *the whole same-instant group as one atomic
    /// unit*, as in [`web_pub_bursts`](Self::web_pub_bursts).
    ///
    /// # Errors
    ///
    /// As [`WebSinkOps::web_pub`].
    fn web_pub_each(&self, server: &WebServer, topic: impl Into<String>) -> Result<Stream<()>>;
}

impl<T> WebBurstSinkOps for Stream<Burst<T>>
where
    T: Serialize + Clone + Default + Send + 'static,
{
    fn web_pub_bursts(&self, server: &WebServer, topic: impl Into<String>) -> Result<Stream<()>> {
        if server.is_historical_noop() {
            return Ok(noop_sink(self));
        }
        let framed = self
            .with_time()
            .map(|(time, values): &(NanoTime, Burst<T>)| burst![(*time, values.to_vec())]);
        publish_frames(&framed, server, topic.into())
    }

    fn web_pub_each(&self, server: &WebServer, topic: impl Into<String>) -> Result<Stream<()>> {
        if server.is_historical_noop() {
            return Ok(noop_sink(self));
        }
        // One `(time, value)` pair per burst entry. `publish_frames` already
        // feeds the burst to its consumer one entry at a time, so this is
        // exactly the framing `web_pub` produces — just N of them.
        let framed = self
            .with_time()
            .map(|(time, values): &(NanoTime, Burst<T>)| {
                values
                    .iter()
                    .map(|value| (*time, value.clone()))
                    .collect::<Burst<(NanoTime, T)>>()
            });
        publish_frames(&framed, server, topic.into())
    }
}

/// The historical-no-op sink: drain the stream without touching the network, so
/// a backtest that does not want a server runs the same graph unmodified.
fn noop_sink<T: Clone + Default + 'static>(stream: &Stream<T>) -> Stream<()> {
    stream.for_each(|_| Ok(()))
}

/// The shared publish path: encode each `(time, payload)` into an [`Envelope`]
/// off the graph thread and deliver it, then emit [`ControlMessage::Complete`]
/// once every queued frame has been flushed.
///
/// `P` is the already-shaped wire payload — `T` for a scalar stream, `Vec<T>`
/// for a burst stream — so the two public entry points differ only in that
/// mapping.
///
/// # The two delivery paths
///
/// Both run on the consumer task, and which one a frame takes is a single
/// atomic load resolved once per run (see [`Delivery`](super::Delivery)):
///
/// * **Lossy** — `try_send` into every subscribed connection's outbound queue,
///   dropping the frame for anyone whose queue is full. Never waits, which is
///   what real time resolves to.
/// * **Lossless** — *await* a slot in each of those queues instead, so the
///   consumer task stalls behind the slowest client. That stall reaches the
///   graph through the bounded sink channel: a stalled consumer stops draining
///   it, it fills, and `spawn_sink`'s `send_blocking` parks the graph thread.
///   With no subscribers nothing is ever waited on, so an unwatched replay runs
///   at full speed.
fn publish_frames<P>(
    framed: &Stream<Burst<(NanoTime, P)>>,
    server: &WebServer,
    topic: String,
) -> Result<Stream<()>>
where
    P: Serialize + Clone + Default + Send + 'static,
{
    let codec = server.codec();
    let complete_topic = topic.clone();

    let graph = framed.graph();
    // The same runtime `consume_async_bursts` spawns its consumer onto — used
    // here only for the teardown delivery of `Complete`.
    let handle = graph.async_runtime_handle()?;

    // The resolved policy for *this publish*, written once at graph `start`.
    //
    // It lives here rather than on the shared `WebServerInner` so that a server
    // serving several graphs cannot have one run overwrite another's policy —
    // flipping a live graph into pacing on browser sockets, or an in-flight
    // replay back to lossy. `false` (lossy) until `start` resolves it, so a
    // publish before then behaves as it always did.
    let lossless = Arc::new(AtomicBool::new(false));
    let consumer_lossless = lossless.clone();
    let consumer_inner = server.inner.clone();
    let consumer_topic = topic.clone();

    // The encode + delivery run on the consumer task, not the graph thread: the
    // registry is behind a mutex, which must never be touched from a cycle (the
    // no-locks-on-the-graph-path invariant), and the lossless path additionally
    // *awaits*, which a cycle cannot do at all.
    //
    // A whole instant at a time (rather than per value) so the subscriber
    // snapshot is taken once per instant rather than once per frame.
    //
    // The channel is **bounded**, and that bound is the whole pacing mechanism:
    // `spawn_sink`'s `send_blocking` parks the graph thread when it is full, so
    // a lossless consumer waiting on a slow socket back-pressures the graph
    // with no extra machinery. Under lossy the consumer never waits on a client
    // — it drops instead — so it drains at encode speed and the bound is only
    // ever reached by a graph outrunning its own encoder, which is a condition
    // worth bounding rather than absorbing into unbounded memory.
    let (sink, flush) = consume_async_bursts(
        &graph,
        Some(PUBLISH_INFLIGHT_INSTANTS),
        move |items: Vec<(NanoTime, P)>| {
            let inner = consumer_inner.clone();
            let topic = consumer_topic.clone();
            let policy = consumer_lossless.clone();
            async move {
                let lossless = policy.load(Ordering::Acquire);
                deliver_instant(codec, &inner, &topic, items, lossless).await
            }
        },
    )?;

    let complete_inner = server.inner.clone();
    let complete_lossless = lossless.clone();
    let sink_stream = framed.for_each(sink).finally(move |_| {
        // Flush first: it closes the sink channel and joins the consumer task,
        // so every queued data frame is on the wire before the end-of-stream
        // marker follows it.
        flush(&())?;
        let bytes = encode_complete_frame(codec, &complete_topic)?;
        // The consumer task is joined, so this is the only writer left; deliver
        // on the graph thread the same way it would have.
        handle.block_on(complete_inner.deliver(
            &complete_topic,
            bytes,
            complete_lossless.load(Ordering::Acquire),
        ));
        Ok(())
    });

    // Resolve `Delivery::Auto` against the run mode. `start` is the earliest
    // point the run mode exists — the server is built before the graph runs,
    // and one server can serve several runs — and it runs before the first
    // cycle, so no frame is published against an unresolved policy.
    let resolve_inner = server.inner.clone();
    Ok(sink_stream.wire(|b, h| {
        b.compose_spawn_at_start(h.index(), move |run_mode, _run_for, _start_time| {
            lossless.store(resolve_inner.resolve_delivery(run_mode), Ordering::Release);
            Ok(StopHandle::new(()))
        });
        h
    }))
}

/// Encode and deliver one instant's frames, in order, by the policy in force.
///
/// The subscriber set is snapshotted **once for the instant**, not once per
/// frame: this is the path that sets a replay's throughput ceiling, and
/// re-locking the registry and cloning its `Vec` per frame would put a mutex
/// acquisition and an allocation on every one. From the publisher's side the
/// set can only usefully change *between* instants — a subscription that goes
/// away mid-instant surfaces as a send failure, and `deliver_to` withdraws it
/// from both the snapshot and the registry.
async fn deliver_instant<P>(
    codec: CodecKind,
    inner: &WebServerInner,
    topic: &str,
    items: Vec<(NanoTime, P)>,
    lossless: bool,
) -> Result<()>
where
    P: Serialize,
{
    let mut targets = inner.targets(topic);
    for (time, payload) in items {
        let env = Envelope {
            topic: topic.to_string(),
            time_ns: u64::from(time),
            payload: codec.encode(&payload)?,
        };
        let bytes = Bytes::from(codec.encode(&env)?);
        inner.deliver_to(topic, &mut targets, bytes, lossless).await;
    }
    Ok(())
}

/// Encode a [`ControlMessage::Complete`] as a control-topic [`Envelope`] ready
/// deliver on a publish topic. It is addressed to
/// [`CONTROL_TOPIC`] so the browser client routes it through its control
/// handler, while riding the publish topic's fan-out so only clients
/// subscribed to that topic receive it.
fn encode_complete_frame(codec: CodecKind, topic: &str) -> Result<Bytes> {
    let ctrl = ControlMessage::Complete {
        topic: topic.to_string(),
    };
    let env = Envelope {
        topic: CONTROL_TOPIC.to_string(),
        time_ns: 0,
        payload: codec.encode(&ctrl)?,
    };
    Ok(Bytes::from(codec.encode(&env)?))
}
