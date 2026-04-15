//! `web_pub` — stream values out to connected WebSocket clients.
//!
//! Each call to [`web_pub`] (or the fluent [`WebPubOperators::web_pub`])
//! registers a topic on the [`WebServer`]. Every value emitted upstream
//! is serialized with the server's codec, wrapped in an [`Envelope`],
//! and broadcast to every WebSocket connection subscribed to that topic.
//!
//! Slow consumers **do not** back-pressure the graph: each client has a
//! bounded outbound queue and a lossy broadcast receiver, so a frozen
//! browser tab simply drops frames.

use std::pin::Pin;
use std::rc::Rc;

use axum::body::Bytes;
use futures::StreamExt;
use serde::Serialize;

use super::codec::Envelope;
use super::server::WebServer;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;

/// Publish every upstream value on `topic`.
///
/// In historical mode ([`WebServer::is_historical_noop`]) the consumer
/// drains the source without sending anything on the wire, so the graph
/// still runs to completion.
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
                let payload = codec.encode(&value)?;
                let env = Envelope {
                    topic: topic.clone(),
                    time_ns: u64::from(time),
                    payload,
                };
                let bytes = Bytes::from(codec.encode(&env)?);
                // `send` only errors when there are zero receivers — fine.
                let _ = sender.send(bytes);
            }
            Ok(())
        },
    ))
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
