//! Redis Streams adapter — snapshot + tail reads (`XRANGE`/`XREAD`) and appends (`XADD`).

use super::{RedisConnection, RedisStreamEvent, RedisStreamRecord};
use crate::burst;
use crate::nodes::{FutStream, RunParams, StreamOperators, produce_async};
use crate::types::*;
use futures::StreamExt;
use redis::AsyncCommands;
use redis::streams::{StreamId, StreamRangeReply, StreamReadOptions, StreamReadReply};
use std::pin::Pin;
use std::rc::Rc;

/// Convert a redis `StreamId` (id + field map) into a [`RedisStreamEvent`].
fn to_event(key: &str, id: &StreamId) -> RedisStreamEvent {
    let fields = id
        .map
        .iter()
        .filter_map(|(name, value)| {
            redis::from_redis_value::<Vec<u8>>(value)
                .ok()
                .map(|bytes| (name.clone(), bytes))
        })
        .collect();
    RedisStreamEvent {
        key: key.to_string(),
        id: id.id.clone(),
        fields,
    }
}

/// Read a Redis stream `key`: emit a snapshot of all existing entries, then tail
/// live appends as [`RedisStreamEvent`]s.
///
/// The snapshot is read with `XRANGE key - +` and its last entry ID is captured;
/// the tail then issues `XREAD BLOCK 0 STREAMS key <last_id>`, which only returns
/// entries with an ID strictly greater than `last_id`. Because the tail reads from
/// the exact snapshot boundary, no entry is missed or duplicated in the handoff.
///
/// Emits `Burst<RedisStreamEvent>`. Use `.collapse()` for single-event processing.
///
/// Designed for `RunMode::RealTime`. In `HistoricalFrom` mode timestamps are wall-clock
/// `NanoTime::now()` rather than historical.
#[must_use]
pub fn redis_stream_read(
    connection: impl Into<RedisConnection>,
    key: impl Into<String>,
) -> Rc<dyn Stream<Burst<RedisStreamEvent>>> {
    let connection = connection.into();
    let key = key.into();
    produce_async(move |_ctx: RunParams| async move {
        let client = redis::Client::open(connection.url.as_str())
            .map_err(|e| anyhow::anyhow!("redis client open failed: {e}"))?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| anyhow::anyhow!("redis connect failed: {e}"))?;

        // 1. Snapshot all existing entries and capture the last ID.
        let snapshot: StreamRangeReply = conn
            .xrange(&key, "-", "+")
            .await
            .map_err(|e| anyhow::anyhow!("redis xrange on {key:?} failed: {e}"))?;
        // "0" means "from the beginning" — used when the stream is empty so the tail
        // picks up the very first appended entry.
        let mut last_id = snapshot
            .ids
            .last()
            .map(|id| id.id.clone())
            .unwrap_or_else(|| "0".to_string());
        let snapshot_events: Vec<RedisStreamEvent> =
            snapshot.ids.iter().map(|id| to_event(&key, id)).collect();

        Ok(async_stream::stream! {
            // Phase 1: emit the snapshot.
            for event in snapshot_events {
                yield Ok((NanoTime::now(), event));
            }

            // Phase 2: tail live appends from the snapshot boundary.
            let opts = StreamReadOptions::default().block(0);
            loop {
                let reply: redis::RedisResult<StreamReadReply> =
                    conn.xread_options(&[&key], &[&last_id], &opts).await;
                match reply {
                    Ok(reply) => {
                        for stream_key in &reply.keys {
                            for id in &stream_key.ids {
                                last_id = id.id.clone();
                                yield Ok((NanoTime::now(), to_event(&stream_key.key, id)));
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("redis xread on {key:?} failed: {e}"));
                        break;
                    }
                }
            }
        })
    })
}

/// Append a `Burst<RedisStreamRecord>` stream to Redis via `XADD`.
///
/// Connects once at startup and issues one `XADD <key> *` per [`RedisStreamRecord`]
/// in each burst, letting Redis assign the entry ID. Any append error terminates the
/// consumer and propagates to the graph.
#[must_use]
pub fn redis_stream_write(
    connection: impl Into<RedisConnection>,
    upstream: &Rc<dyn Stream<Burst<RedisStreamRecord>>>,
) -> Rc<dyn Node> {
    let connection = connection.into();
    upstream.consume_async(Box::new(
        move |_ctx: RunParams,
              mut source: Pin<Box<dyn FutStream<Burst<RedisStreamRecord>>>>| async move {
            let client = redis::Client::open(connection.url.as_str())
                .map_err(|e| anyhow::anyhow!("redis client open failed: {e}"))?;
            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| anyhow::anyhow!("redis connect failed: {e}"))?;

            while let Some((_time, burst)) = source.next().await {
                for record in burst {
                    let mut cmd = redis::cmd("XADD");
                    cmd.arg(record.key.as_str()).arg("*");
                    for (field, value) in &record.fields {
                        cmd.arg(field.as_str()).arg(value.as_slice());
                    }
                    cmd.query_async::<String>(&mut conn).await.map_err(|e| {
                        anyhow::anyhow!("redis xadd to {:?} failed: {e}", record.key)
                    })?;
                }
            }
            Ok(())
        },
    ))
}

/// Extension trait providing a fluent API for appending streams to Redis streams.
///
/// Implemented for both `Burst<RedisStreamRecord>` (multi-item) and `RedisStreamRecord`
/// (single-item) streams, so burst wrapping is never required in user code.
pub trait RedisStreamOperators {
    /// Append this stream to a Redis stream via `XADD`.
    #[must_use]
    fn redis_stream_write(self: &Rc<Self>, conn: impl Into<RedisConnection>) -> Rc<dyn Node>;
}

impl RedisStreamOperators for dyn Stream<Burst<RedisStreamRecord>> {
    fn redis_stream_write(self: &Rc<Self>, conn: impl Into<RedisConnection>) -> Rc<dyn Node> {
        redis_stream_write(conn, self)
    }
}

impl RedisStreamOperators for dyn Stream<RedisStreamRecord> {
    fn redis_stream_write(self: &Rc<Self>, conn: impl Into<RedisConnection>) -> Rc<dyn Node> {
        let burst_stream = self.map(|record| burst![record]);
        redis_stream_write(conn, &burst_stream)
    }
}
