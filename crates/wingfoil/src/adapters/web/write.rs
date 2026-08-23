//! `web_pub` — stream values out to connected WebSocket clients.
//!
//! Each call to [`WebSinkOps::web_pub`] registers a topic on the [`WebServer`].
//! Upstream values are serialized with the server's codec, wrapped in an
//! [`Envelope`], and broadcast to every WebSocket connection subscribed to that
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
//! outbound queue and a lossy broadcast receiver, so a frozen browser tab
//! simply drops frames. In a **historical replay** there is no live clock to
//! fall behind, so the default pace-to-the-slowest-subscriber policy makes the
//! publisher wait instead of corrupting the replay.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use axum::body::Bytes;
use serde::Serialize;
use tokio::sync::Semaphore;
use wingfoil::NanoTime;

use super::codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope};
use super::server::{LosslessTargets, WebServer, WebServerInner};
use crate::Burst;
use crate::async_source::consume_async_bursts;
use crate::burst;
use crate::fluent::{Stream, StreamOps};
use crate::interp::StopHandle;

/// How many instants the graph may run ahead of the lossless outbound path
/// before it is made to wait.
///
/// One permit per **burst** (one instant's frames), not per frame: a burst is
/// indivisible on the graph side, and per-frame accounting would deadlock the
/// moment a single instant carried more values than the budget. The publisher
/// takes a permit before handing an instant to the consumer task and the
/// consumer returns it once every frame of that instant has been delivered, so
/// at most this many instants are ever in flight.
///
/// It costs nothing under [`Delivery::Lossy`](super::Delivery::Lossy) — the graph thread does one
/// relaxed atomic load and skips the permit entirely.
const LOSSLESS_INFLIGHT_INSTANTS: usize = 64;

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
    /// run ends, a [`ControlMessage::Complete`] is broadcast on the topic so
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
    /// be created. A codec failure aborts the run with context; a broadcast with
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
/// * **Lossy** — `broadcast::send`, which never blocks and drops for any
///   receiver that has fallen behind. Byte-for-byte the pre-`Delivery`
///   behaviour, and what real time still resolves to.
/// * **Lossless** — await a send into every subscribed connection's outbound
///   queue, so the consumer task stalls behind the slowest client. That stall
///   reaches the graph through the `LOSSLESS_INFLIGHT_INSTANTS` semaphore
///   below: the graph thread takes a permit per instant and the consumer
///   returns it once that instant is delivered, so the graph runs at most that
///   many instants ahead and then waits. With no subscribers nothing is ever
///   waited on, so an unwatched replay runs at full speed.
fn publish_frames<P>(
    framed: &Stream<Burst<(NanoTime, P)>>,
    server: &WebServer,
    topic: String,
) -> Result<Stream<()>>
where
    P: Serialize + Clone + Default + Send + 'static,
{
    let codec = server.codec();
    // Pre-register the broadcast sender so clients that connect before the
    // first tick still see subsequent frames.
    let sender = server.inner.get_or_create_pub_topic(&topic);
    let complete_sender = sender.clone();
    let complete_topic = topic.clone();

    let graph = framed.graph();
    // The same runtime `consume_async_bursts` spawns its consumer onto — used
    // here only to park the graph thread on a permit, and on the teardown
    // delivery of `Complete`.
    let handle = graph.async_runtime_handle()?;

    let inflight = Arc::new(Semaphore::new(LOSSLESS_INFLIGHT_INSTANTS));
    // The resolved policy for *this publish*, written once at graph `start`.
    //
    // It lives here rather than on the shared `WebServerInner` so that the
    // graph thread and the consumer task provably read the same value: one
    // `WebServer` can serve several graphs, and two of them running at once in
    // different run modes would otherwise overwrite each other's policy —
    // flipping a live graph into pacing on browser sockets, or an in-flight
    // replay back to lossy, and unbalancing the semaphore either way. `false`
    // (lossy) until `start` resolves it, so a publish before then behaves as it
    // always did.
    let lossless = Arc::new(AtomicBool::new(false));
    let consumer_lossless = lossless.clone();
    let consumer_inflight = inflight.clone();
    let consumer_inner = server.inner.clone();
    let consumer_topic = topic.clone();

    // The encode + delivery run on the consumer task, not the graph thread:
    // `broadcast::send` takes an internal lock, which must never be touched
    // from a cycle (the no-locks-on-the-graph-path invariant), and the lossless
    // path additionally *awaits*, which a cycle cannot do at all.
    //
    // A whole instant at a time (rather than per value) so the permit the graph
    // took for that instant is returned exactly once, after its last frame.
    let (sink, flush) = consume_async_bursts(&graph, None, move |items: Vec<(NanoTime, P)>| {
        let inner = consumer_inner.clone();
        let sender = sender.clone();
        let topic = consumer_topic.clone();
        let inflight = consumer_inflight.clone();
        let policy = consumer_lossless.clone();
        async move {
            // Read the policy once, and return a permit only if one was taken.
            // The graph thread reads this same per-publish cell, and it is
            // written only at `start` — before any cycle, with the previous
            // run's consumer already joined — so the two sides cannot disagree
            // about an instant. Adding a permit the graph never took would
            // inflate the semaphore for the rest of the run.
            let lossless = policy.load(Ordering::Acquire);
            let result = deliver_instant(codec, &inner, &sender, &topic, items, lossless).await;
            if lossless {
                // Hand it back whatever happened: on the error path the consumer
                // task is about to stop, and a graph thread parked on `acquire`
                // would otherwise never learn that.
                inflight.add_permits(1);
            }
            result
        }
    })?;

    let pacer_lossless = lossless.clone();
    let pacer_handle = handle.clone();
    let paced_sink = move |values: &Burst<(NanoTime, P)>| -> Result<()> {
        // `consume_async_bursts` skips empty bursts, so its consumer would
        // never return a permit taken for one. Skip them here too.
        if values.is_empty() {
            return Ok(());
        }
        let paced = pacer_lossless.load(Ordering::Acquire);
        if paced {
            // Fast path: a permit is free, take it without touching the
            // runtime. Only a full pipeline parks the graph thread.
            match inflight.try_acquire() {
                Ok(permit) => permit.forget(),
                Err(_) => pacer_handle
                    .block_on(inflight.acquire())
                    .map_err(|_| anyhow::anyhow!("web_pub: delivery pacer closed"))?
                    .forget(),
            }
        }
        let handed_over = sink(values);
        if paced && handed_over.is_err() {
            // The instant never reached the consumer, so nothing will return
            // its permit. Give it back here — a permit leaked per aborted run
            // would eventually starve a re-run of the same graph.
            inflight.add_permits(1);
        }
        handed_over
    };

    let complete_inner = server.inner.clone();
    let complete_lossless = lossless.clone();
    let sink_stream = framed.for_each(paced_sink).finally(move |_| {
        // Flush first: it closes the sink channel and joins the consumer task,
        // so every queued data frame is on the wire before the end-of-stream
        // marker follows it.
        flush(&())?;
        let bytes = encode_complete_frame(codec, &complete_topic)?;
        if complete_lossless.load(Ordering::Acquire) {
            // The consumer task is joined, so this is the only writer left;
            // deliver on the graph thread the same way it would have.
            handle.block_on(complete_inner.deliver_lossless(&complete_topic, bytes));
        } else {
            let _ = complete_sender.send(bytes);
        }
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
/// Split out of the consumer closure so the permit hand-back above covers the
/// error path as well as the happy one.
///
/// The lossless path snapshots its subscribers **once for the instant**, not
/// once per frame: this is the path that now sets a replay's throughput
/// ceiling, and re-locking the registry and cloning its `Vec` per frame would
/// put a mutex acquisition and an allocation on every one. From the
/// publisher's side the subscriber set can only usefully change *between*
/// instants — a subscription that goes away mid-instant surfaces as a send
/// failure, and `deliver_lossless_to` withdraws it from both the snapshot and
/// the registry.
async fn deliver_instant<P>(
    codec: CodecKind,
    inner: &WebServerInner,
    sender: &tokio::sync::broadcast::Sender<Bytes>,
    topic: &str,
    items: Vec<(NanoTime, P)>,
    lossless: bool,
) -> Result<()>
where
    P: Serialize,
{
    let mut targets = if lossless {
        inner.lossless_targets(topic)
    } else {
        LosslessTargets::new()
    };
    for (time, payload) in items {
        let env = Envelope {
            topic: topic.to_string(),
            time_ns: u64::from(time),
            payload: codec.encode(&payload)?,
        };
        let bytes = Bytes::from(codec.encode(&env)?);
        if lossless {
            inner.deliver_lossless_to(topic, &mut targets, bytes).await;
        } else {
            // `send` only errors when there are zero receivers — fine, a
            // graph may publish with no clients connected.
            let _ = sender.send(bytes);
        }
    }
    Ok(())
}

/// Encode a [`ControlMessage::Complete`] as a control-topic [`Envelope`] ready
/// to broadcast on a publish topic's channel. It is addressed to
/// [`CONTROL_TOPIC`] so the browser client routes it through its control
/// handler, while riding the publish topic's broadcast so only clients
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
