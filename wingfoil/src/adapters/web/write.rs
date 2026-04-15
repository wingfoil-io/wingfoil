//! `web_pub` — stream values out to connected WebSocket clients.
//!
//! Each call to [`web_pub`] (or the fluent [`WebPubOperators::web_pub`])
//! registers a topic on the [`WebServer`]. Every value emitted upstream
//! is serialized with the server's codec, wrapped in an [`Envelope`],
//! and broadcast to every WebSocket connection subscribed to that topic.
//!
//! Slow consumers **do not** back-pressure the graph: each client has a
//! bounded outbound queue and a lossy broadcast receiver, so a frozen
//! browser tab simply drops frames. See [`super::server`] for capacity
//! constants.

use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;

use super::codec::Envelope;
use super::server::WebServer;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;

/// Publish every upstream value on `topic`.
///
/// The closure serializes one [`Envelope`] per tick using the server's
/// [`Codec`](super::codec::Codec) and hands the bytes to the per-topic
/// broadcast channel.  The WebSocket connection tasks drain the
/// broadcast and forward frames to clients that have subscribed to
/// this topic.
///
/// In historical mode ([`WebServer::is_historical_noop`]) the consumer
/// becomes a no-op — values are drained but nothing is sent on the
/// wire, mirroring the Prometheus exporter's historical behaviour.
#[must_use]
pub fn web_pub<T: Element + Send + Serialize>(
    server: &WebServer,
    topic: impl Into<String>,
    upstream: &Rc<dyn Stream<T>>,
) -> Rc<dyn Node> {
    let topic = topic.into();
    let codec = server.codec();
    let inner = server.inner.clone();
    let historical = server.is_historical_noop();

    // Pre-register the broadcast sender. This is important because a
    // client may connect and subscribe to `topic` before the graph
    // starts ticking — without the channel existing, the subscription
    // would see no frames until the first tick.
    let sender = inner.get_or_create_pub_topic(&topic);

    upstream.consume_async(Box::new(
        move |_ctx: RunParams, source: Pin<Box<dyn FutStream<T>>>| async move {
            if historical {
                // Drain the stream but don't publish — keeps the graph
                // running and means tests can exercise pub sinks in
                // HistoricalFrom mode without opening sockets.
                let mut source = source;
                while source.next().await.is_some() {}
                return Ok(());
            }
            let mut source = source;
            while let Some((time, value)) = source.next().await {
                let payload = codec.encode_payload(&value)?;
                let env = Envelope {
                    topic: topic.clone(),
                    time_ns: u64::from(time),
                    payload,
                };
                let bytes = codec.encode_envelope(&env)?;
                // `send` only errors when there are zero receivers —
                // that's fine (no clients currently connected); just
                // ignore it.
                let _ = sender.send(Arc::new(bytes));
            }
            Ok(())
        },
    ))
}

/// Extension trait giving the fluent `.web_pub(...)` call on streams.
///
/// Implemented for both single-value streams and `Burst<T>` streams so
/// no manual wrapping is needed at the call site.
pub trait WebPubOperators<T: Element + Send + Serialize> {
    /// Publish this stream to `topic` on `server`.
    #[must_use]
    fn web_pub(self: &Rc<Self>, server: &WebServer, topic: impl Into<String>) -> Rc<dyn Node>;
}

impl<T: Element + Send + Serialize> WebPubOperators<T> for dyn Stream<T> {
    fn web_pub(self: &Rc<Self>, server: &WebServer, topic: impl Into<String>) -> Rc<dyn Node> {
        web_pub(server, topic, self)
    }
}
