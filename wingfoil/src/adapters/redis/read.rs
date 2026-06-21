//! Redis Pub/Sub producer — subscribes to a channel and streams messages.

use super::{RedisConnection, RedisEvent};
use crate::nodes::{RunParams, produce_async};
use crate::types::*;
use futures::StreamExt;
use std::rc::Rc;

/// Subscribe to a Redis `channel` and stream every message published to it as a [`RedisEvent`].
///
/// Connects and issues `SUBSCRIBE` once at startup, then emits each incoming message.
/// Redis Pub/Sub has no backlog: only messages published *after* the subscription is
/// registered are delivered. Any connection or subscribe error terminates the producer
/// and propagates to the graph.
///
/// Emits `Burst<RedisEvent>`. Use `.collapse()` for single-event processing.
///
/// Designed for `RunMode::RealTime`. In `HistoricalFrom` mode timestamps are wall-clock
/// `NanoTime::now()` rather than historical, since messages arrive live off the wire.
#[must_use]
pub fn redis_sub(
    connection: impl Into<RedisConnection>,
    channel: impl Into<String>,
) -> Rc<dyn Stream<Burst<RedisEvent>>> {
    let connection = connection.into();
    let channel = channel.into();
    produce_async(move |_ctx: RunParams| async move {
        let client = redis::Client::open(connection.url.as_str())
            .map_err(|e| anyhow::anyhow!("redis client open failed: {e}"))?;
        let mut pubsub = client
            .get_async_pubsub()
            .await
            .map_err(|e| anyhow::anyhow!("redis connect failed: {e}"))?;
        pubsub
            .subscribe(channel.as_str())
            .await
            .map_err(|e| anyhow::anyhow!("redis subscribe to {channel:?} failed: {e}"))?;

        Ok(async_stream::stream! {
            let mut messages = pubsub.on_message();
            while let Some(msg) = messages.next().await {
                let event = RedisEvent {
                    channel: msg.get_channel_name().to_string(),
                    payload: msg.get_payload_bytes().to_vec(),
                };
                yield Ok((NanoTime::now(), event));
            }
            // `on_message` ends only when the connection drops.
            yield Err(anyhow::anyhow!("redis subscription stream closed unexpectedly"));
        })
    })
}
