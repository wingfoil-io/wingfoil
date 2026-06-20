//! Redis Pub/Sub consumer — publishes upstream messages via `PUBLISH`.

use super::{RedisConnection, RedisEntry};
use crate::burst;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;
use futures::StreamExt;
use std::pin::Pin;
use std::rc::Rc;

/// Publish a `Burst<RedisEntry>` stream to Redis via `PUBLISH`.
///
/// Connects once at startup and issues one `PUBLISH` per [`RedisEntry`] in each burst,
/// to the channel each entry names. Any publish error terminates the consumer and
/// propagates to the graph.
///
/// Redis Pub/Sub is fire-and-forget: a published message reaches only the clients that
/// are subscribed at the moment of publication. `PUBLISH` returns the number of clients
/// that received the message (which may be zero); this count is not surfaced.
#[must_use]
pub fn redis_pub(
    connection: impl Into<RedisConnection>,
    upstream: &Rc<dyn Stream<Burst<RedisEntry>>>,
) -> Rc<dyn Node> {
    let connection = connection.into();
    upstream.consume_async(Box::new(
        move |_ctx: RunParams, mut source: Pin<Box<dyn FutStream<Burst<RedisEntry>>>>| async move {
            let client = redis::Client::open(connection.url.as_str())
                .map_err(|e| anyhow::anyhow!("redis client open failed: {e}"))?;
            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| anyhow::anyhow!("redis connect failed: {e}"))?;

            while let Some((_time, burst)) = source.next().await {
                for entry in burst {
                    redis::cmd("PUBLISH")
                        .arg(entry.channel.as_str())
                        .arg(entry.payload.as_slice())
                        .query_async::<i64>(&mut conn)
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!("redis publish to {:?} failed: {e}", entry.channel)
                        })?;
                }
            }
            Ok(())
        },
    ))
}

/// Extension trait providing a fluent API for publishing streams to Redis.
///
/// Implemented for both `Burst<RedisEntry>` (multi-item) and `RedisEntry` (single-item)
/// streams, so burst wrapping is never required in user code.
pub trait RedisPubOperators {
    /// Publish this stream to Redis via `PUBLISH`.
    #[must_use]
    fn redis_pub(self: &Rc<Self>, conn: impl Into<RedisConnection>) -> Rc<dyn Node>;
}

impl RedisPubOperators for dyn Stream<Burst<RedisEntry>> {
    fn redis_pub(self: &Rc<Self>, conn: impl Into<RedisConnection>) -> Rc<dyn Node> {
        redis_pub(conn, self)
    }
}

impl RedisPubOperators for dyn Stream<RedisEntry> {
    fn redis_pub(self: &Rc<Self>, conn: impl Into<RedisConnection>) -> Rc<dyn Node> {
        let burst_stream = self.map(|entry| burst![entry]);
        redis_pub(conn, &burst_stream)
    }
}
