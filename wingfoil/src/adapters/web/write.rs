//! `web_pub` — stream values out to connected WebSocket clients.
//!
//! Each call to [`web_pub`] (or the fluent [`WebPubOperators::web_pub`])
//! registers a topic on the [`WebServer`]. Upstream values are serialized
//! with the server's codec, wrapped in an [`Envelope`], and broadcast to
//! every WebSocket connection subscribed to that topic.
//!
//! Each envelope payload is a **burst** — the sequence of values that share
//! one `time_ns` — matching the [`Burst<T>`] the graph engine already deals
//! in. On the wire the payload therefore always decodes to an *array*, and
//! the browser client decides whether to collapse it to the latest value or
//! consume the whole group. In real time each frame carries a single-element
//! burst (emitted immediately, no batching latency); in historical mode
//! consecutive same-`time_ns` values are grouped into one atomic frame, so a
//! lossy drop can never split a timestamp and a replay stays coherent.
//!
//! Slow consumers **do not** back-pressure the graph: each client has a
//! bounded outbound queue and a lossy broadcast receiver, so a frozen
//! browser tab simply drops frames.

use std::pin::Pin;
use std::rc::Rc;

use axum::body::Bytes;
use futures::StreamExt;
use serde::Serialize;

use super::codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope};
use super::server::WebServer;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;

/// Publish every upstream value on `topic`.
///
/// Works identically under `RunMode::RealTime` and
/// `RunMode::HistoricalFrom` — a historical replay (or any finite
/// `RunFor`) streams its values out to subscribed clients just like a
/// live run, which is what powers browser-side visualisation of a
/// backtest / slow computation. Each value is sent immediately (no
/// batching latency). When the upstream source ends (source exhausted or
/// the `RunFor` limit reached) a [`ControlMessage::Complete`] is broadcast
/// so clients know the stream is finished rather than merely dropped.
///
/// # Bursts
///
/// The payload is the codec-serialized upstream value. A scalar `T`
/// (number, struct, …) is a single JSON/bincode value; the browser client
/// treats it as a one-element burst. Publishing a stream whose value is a
/// [`Burst<T>`] (e.g. straight from [`web_sub`](super::web_sub) or an
/// async source) serializes it as an **array**, which the client surfaces
/// as the whole same-`time_ns` group — atomic on the wire, so a lossy drop
/// can never split a timestamp. See `subscribe` / `subscribeBurst` in
/// `@wingfoil/client`.
///
/// The exception is a server built with
/// [`WebServerBuilder::start_historical`](super::WebServerBuilder::start_historical),
/// whose [`WebServer::is_historical_noop`] flag makes both `web_pub` and
/// `web_sub` no-ops: the consumer drains the source without touching the
/// network, so a backtest that does *not* want a server can run the same
/// graph unmodified.
///
/// Back-pressure note: subscribed clients never stall the graph — each
/// client has a bounded, lossy outbound path (see the server broadcast
/// buffer), so a client that cannot keep up drops frames. For a faithful,
/// loss-free replay, pace the graph so it does not outrun the client
/// (e.g. a genuinely compute-bound historical run).
#[must_use]
pub fn web_pub<T: Element + Send + Serialize>(
    server: &WebServer,
    topic: impl Into<String>,
    upstream: &Rc<dyn Stream<T>>,
) -> Rc<dyn Node> {
    let topic = topic.into();
    let codec = server.codec();
    let historical = server.is_historical_noop();

    // Pre-register the broadcast sender so clients that connect before
    // the first tick still see subsequent frames.
    let sender = server.inner.get_or_create_pub_topic(&topic);

    upstream.consume_async(Box::new(
        move |_ctx: RunParams, source: Pin<Box<dyn FutStream<T>>>| async move {
            let mut source = source;
            if historical {
                // consume_async treats an early Ok(()) as an error, so drain
                // even though we have no wire to push to.
                while source.next().await.is_some() {}
                return Ok(());
            }
            while let Some((time, value)) = source.next().await {
                let env = Envelope {
                    topic: topic.clone(),
                    time_ns: u64::from(time),
                    payload: codec.encode(&value)?,
                };
                let bytes = Bytes::from(codec.encode(&env)?);
                // `send` only errors when there are zero receivers — fine.
                let _ = sender.send(bytes);
            }
            // Source is exhausted (finite RunFor / end of historical
            // replay). Emit a clean end-of-stream marker so subscribers
            // can distinguish "replay finished" from a transport drop.
            let bytes = encode_complete_frame(codec, &topic)?;
            let _ = sender.send(bytes);
            Ok(())
        },
    ))
}

/// Encode a [`ControlMessage::Complete`] as a control-topic [`Envelope`]
/// ready to broadcast on a publish topic's channel. It is addressed to
/// [`CONTROL_TOPIC`] so the browser client routes it through its control
/// handler, while riding the publish topic's broadcast so only clients
/// subscribed to that topic receive it.
fn encode_complete_frame(codec: CodecKind, topic: &str) -> anyhow::Result<Bytes> {
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

/// Fluent `.web_pub(...)` on streams.
pub trait WebPubOperators<T: Element + Send + Serialize> {
    #[must_use]
    fn web_pub(self: &Rc<Self>, server: &WebServer, topic: impl Into<String>) -> Rc<dyn Node>;
}

impl<T: Element + Send + Serialize> WebPubOperators<T> for dyn Stream<T> {
    fn web_pub(self: &Rc<Self>, server: &WebServer, topic: impl Into<String>) -> Rc<dyn Node> {
        web_pub(server, topic, self)
    }
}
