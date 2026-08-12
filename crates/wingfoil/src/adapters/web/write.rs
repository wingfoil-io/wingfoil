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
//! Slow consumers **do not** back-pressure the graph: each client has a bounded
//! outbound queue and a lossy broadcast receiver, so a frozen browser tab simply
//! drops frames.

use anyhow::Result;
use axum::body::Bytes;
use serde::Serialize;
use wingfoil::NanoTime;

use super::codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope};
use super::server::WebServer;
use crate::Burst;
use crate::async_source::consume_async;
use crate::burst;
use crate::fluent::{Stream, StreamOps};

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
    /// Back-pressure note: subscribed clients never stall the graph — each
    /// client has a bounded, lossy outbound path, so a client that cannot keep
    /// up drops frames. For a faithful, loss-free replay, pace the graph so it
    /// does not outrun the client (e.g. a genuinely compute-bound historical
    /// run).
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
    /// Atomic on the wire, so a lossy drop can never split a timestamp; the
    /// client surfaces the group whole via `subscribeBurst`. Semantics are
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
    /// Frames within one burst are broadcast in burst order and all carry the
    /// same `time_ns`. Unlike `web_pub_bursts` they are not atomic on the wire,
    /// so a lossy client drop can split a group; take that trade only when the
    /// client cannot be changed.
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
/// off the graph thread and broadcast it, then emit
/// [`ControlMessage::Complete`] once every queued frame has been flushed.
///
/// `P` is the already-shaped wire payload — `T` for a scalar stream, `Vec<T>`
/// for a burst stream — so the two public entry points differ only in that
/// mapping.
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

    // The encode + broadcast run on the consumer task, not the graph thread:
    // `broadcast::send` takes an internal lock, which must never be touched
    // from a cycle (the no-locks-on-the-graph-path invariant).
    let (sink, flush) = consume_async(
        &framed.graph(),
        None,
        move |(time, payload): (NanoTime, P)| {
            let sender = sender.clone();
            let topic = topic.clone();
            async move {
                let env = Envelope {
                    topic,
                    time_ns: u64::from(time),
                    payload: codec.encode(&payload)?,
                };
                let bytes = Bytes::from(codec.encode(&env)?);
                // `send` only errors when there are zero receivers — fine, a
                // graph may publish with no clients connected.
                let _ = sender.send(bytes);
                Ok(())
            }
        },
    )?;

    Ok(framed.for_each(sink).finally(move |_| {
        // Flush first: it closes the sink channel and joins the consumer task,
        // so every queued data frame is on the wire before the end-of-stream
        // marker rides the same broadcast channel behind it.
        flush(&())?;
        let bytes = encode_complete_frame(codec, &complete_topic)?;
        let _ = complete_sender.send(bytes);
        Ok(())
    }))
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
