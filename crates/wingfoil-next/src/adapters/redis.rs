//! redis adapter — Pub/Sub (`SUBSCRIBE`/`PUBLISH`) and Streams
//! (`XRANGE`/`XREAD`/`XADD`) transports for Redis. It ports the legacy
//! `wingfoil::adapters::redis` module onto the Op model.
//!
//! Two transports, each with a source and a sink:
//!
//! - **Pub/Sub** (fire-and-forget channels): [`redis_sub`] subscribes to a
//!   channel and emits each message; [`RedisSinkOps::redis_pub`] publishes.
//! - **Streams** (a persistent, replayable log): [`redis_stream_read`] replays a
//!   snapshot of existing entries then tails live appends;
//!   [`RedisStreamSinkOps::redis_stream_write`] appends via `XADD`.
//!
//! `HSET`/key-value operations are intentionally out of scope, matching legacy.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Sources** — the free builder functions [`redis_sub`] and
//!   [`redis_stream_read`] on a [`GraphBuilder`], emitting
//!   `Stream<Burst<RedisEvent>>` / `Stream<Burst<RedisStreamEvent>>`.
//! - **Sinks** — the [`RedisSinkOps`] and [`RedisStreamSinkOps`] extension
//!   traits on `Stream<Burst<T>>` (and, for convenience, `Stream<T>`), enabled
//!   with `use wingfoil_next::adapters::redis::{RedisSinkOps, RedisStreamSinkOps};`.
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* (Pub/Sub sub+pub, Streams snapshot→tail read +
//! append, all four value types) is preserved. The surface differs in three
//! deliberate ways, mirroring the [`etcd`](crate::adapters::etcd) port:
//!
//! 1. **The graph owns the tokio runtime.** Legacy hides a never-dropped global
//!    runtime inside `produce_async`/`consume_async`. Next's `GraphBuilder` owns
//!    one runtime, created lazily on first async use and dropped at teardown,
//!    shared by every async adapter — so the common call needs no `&Handle` and
//!    there is no leaked global (see `docs/runtime-ownership.md`). The sources
//!    take a [`RunMode`] (only to reject a historical run at wiring); the producer
//!    spawns in `start()` and derives its `RunParams` from the actual run. Embed
//!    in an existing runtime by installing an override with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime).
//! 2. **The sinks connect lazily, on the first write.** Like legacy, `redis_pub`
//!    / `redis_stream_write` open the socket inside the async consumer on the
//!    first write (`Client::open` — a pure URL parse — still validates at
//!    wiring), so wiring does no I/O and a connection failure surfaces during
//!    the run (via `consume_async`'s error channel), not at graph construction.
//! 3. **The sinks are traits only.** Legacy exposed free `redis_pub` /
//!    `redis_stream_write` functions *and* operator traits; next folds each into
//!    a single sink trait ([`RedisSinkOps`] / [`RedisStreamSinkOps`]), per the
//!    sink-as-trait convention shared with [`lines`](crate::adapters::lines) /
//!    [`csv`](crate::adapters::csv) / [`etcd`](crate::adapters::etcd).
//!
//! Two behavioural notes carried from the burst model:
//!
//! - **Snapshot bursts.** [`redis_stream_read`]'s snapshot phase stamps every
//!   existing entry with one shared `NanoTime::now()`, so the whole snapshot
//!   rides a single atomic [`Burst`] (never latest-wins, never split across
//!   cycles) — matching the [`etcd`](crate::adapters::etcd) port. Legacy
//!   stamped each snapshot entry with its own `NanoTime::now()`.
//! - **Realtime only.** Both sources are live, unbounded, wall-clock-stamped
//!   streams with no historical timeline to replay (Pub/Sub has no backlog; the
//!   stream tail blocks forever). A `HistoricalFrom` run is **rejected at wiring
//!   time** — the historical channel path would block-collect the whole stream
//!   up front and deadlock at `start` (the same reasoning as `etcd_sub`).
//!
//! ## Runtime requirement (a `block_on` footgun)
//!
//! The sinks drive their writes off the graph thread with
//! [`consume_async`](crate::async_source::consume_async), which uses
//! [`Handle::block_on`](tokio::runtime::Handle::block_on) on the graph thread
//! for back-pressure and the teardown flush. So **the graph must be built, run,
//! and dropped from a non-async thread** (`main`, a `#[test]` fn). Driving it
//! from inside an async context makes those `block_on` calls panic.
//!
//! # Pub/Sub: fire-and-forget (no snapshot)
//!
//! Redis Pub/Sub has **no backlog, replay, or offsets**. A subscriber only
//! receives messages published *after* its `SUBSCRIBE` completes, so there is no
//! snapshot phase. A publisher racing a subscriber must order subscribe before
//! publish (retry until captured, or wait for a ready flag).
//!
//! # Streams: snapshot → tail handoff (race prevention)
//!
//! [`redis_stream_read`] reads the snapshot with `XRANGE key - +`, captures its
//! last entry ID, then tails with `XREAD BLOCK 0 STREAMS key <last_id>` — which
//! only returns entries with an ID strictly greater than `last_id`. Because the
//! tail reads from the exact snapshot boundary, any entry appended between the
//! `XRANGE` and the first `XREAD` is returned by `XREAD` (never missed) and no
//! snapshot entry is re-delivered. This mirrors `etcd_sub`'s watch-before-GET
//! guarantee, with stream IDs instead of etcd revisions.
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 6379:6379 redis:7-alpine
//! ```

use std::sync::Arc;

use anyhow::{Context, Result};
use futures::StreamExt;
use redis::AsyncCommands;
use redis::streams::{StreamId, StreamRangeReply, StreamReadOptions, StreamReadReply};
use wingfoil_next::{NanoTime, RunMode};

use crate::async_source::{RunParams, consume_async, produce_async};
use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::{Burst, burst};

/// Connection configuration for Redis.
#[derive(Debug, Clone)]
pub struct RedisConnection {
    /// Redis connection URL, e.g. `"redis://127.0.0.1:6379"`.
    ///
    /// Supports the standard `redis://`/`rediss://` URL scheme, including an
    /// optional password and database index: `"redis://:pass@host:6379/0"`.
    pub url: String,
}

impl RedisConnection {
    /// Create a connection config from a Redis URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    /// Render the URL with any `user:pass@` / `:pass@` userinfo masked, for safe
    /// inclusion in error messages and logs.
    ///
    /// Redis URLs carry credentials in the authority's userinfo
    /// (`redis://user:pass@host:6379/0`, `rediss://:pass@host`), so the whole
    /// userinfo component is replaced with `***:***` while the scheme, host,
    /// port, and path are preserved. A URL without userinfo is returned
    /// unchanged. Used at every `Client::open` / connect error site so the raw
    /// URL's password never reaches a log or an aborted-run error (parity with
    /// the postgres adapter's `PostgresConnection::redacted`, legacy PR #433).
    #[must_use]
    pub fn redacted(&self) -> String {
        redact_redis_url(&self.url)
    }
}

/// Mask any `user:pass@` / `:pass@` userinfo in a Redis URL, preserving the
/// scheme, host, port, and path. See [`RedisConnection::redacted`].
fn redact_redis_url(url: &str) -> String {
    // Split `scheme://` from the authority (+ optional `/path`).
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    // The authority ends at the first `/` (the db path) if present. The host
    // never contains `@`, so the last `@` in the authority delimits userinfo
    // (robust to an `@` inside the password).
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    match authority.rsplit_once('@') {
        Some((_userinfo, host_port)) => format!("{scheme}://***:***@{host_port}{path}"),
        None => url.to_string(),
    }
}

impl From<&str> for RedisConnection {
    fn from(url: &str) -> Self {
        Self::new(url)
    }
}

impl From<String> for RedisConnection {
    fn from(url: String) -> Self {
        Self::new(url)
    }
}

impl From<&String> for RedisConnection {
    fn from(url: &String) -> Self {
        Self::new(url.clone())
    }
}

/// Open (once) or reuse a sink's multiplexed connection, caching it in `cell`.
///
/// The socket is opened lazily on the consumer task the first time a sink
/// writes — deferred off the wiring path — so a connect failure aborts the
/// *run* (via `consume_async`'s error channel), not graph construction. This
/// matches legacy, which connected lazily inside the async consumer. `who`
/// names the sink for error context.
async fn get_or_connect(
    client: &redis::Client,
    cell: &tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
    redacted_url: &str,
    who: &str,
) -> Result<redis::aio::MultiplexedConnection> {
    let mut guard = cell.lock().await;
    match guard.as_ref() {
        Some(c) => Ok(c.clone()),
        None => {
            let c = client
                .get_multiplexed_async_connection()
                .await
                .with_context(|| format!("{who}: connecting to {redacted_url}"))?;
            *guard = Some(c.clone());
            Ok(c)
        }
    }
}

/// A message to publish to a Redis channel.
#[derive(Debug, Clone, Default)]
pub struct RedisEntry {
    /// The target channel.
    pub channel: String,
    /// The message payload.
    pub payload: Vec<u8>,
}

impl RedisEntry {
    /// Interpret the payload as a UTF-8 string.
    pub fn payload_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.payload)
    }
}

/// A message received from a subscribed Redis channel.
#[derive(Debug, Clone, Default)]
pub struct RedisEvent {
    /// The channel the message was published to.
    pub channel: String,
    /// The message payload.
    pub payload: Vec<u8>,
}

impl RedisEvent {
    /// Interpret the payload as a UTF-8 string.
    pub fn payload_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.payload)
    }
}

/// A record to append to a Redis stream via `XADD`.
///
/// A Redis stream entry is an ordered map of field → value pairs. The entry ID
/// is assigned by Redis (`XADD key *`), so it is not part of the record.
#[derive(Debug, Clone, Default)]
pub struct RedisStreamRecord {
    /// The target stream key.
    pub key: String,
    /// The entry's field → value pairs, in insertion order.
    pub fields: Vec<(String, Vec<u8>)>,
}

impl RedisStreamRecord {
    /// Build a single-field record (`field` → `value`) for stream `key`.
    pub fn single(
        key: impl Into<String>,
        field: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            key: key.into(),
            fields: vec![(field.into(), value.into())],
        }
    }
}

/// An entry read from a Redis stream via `XRANGE` (snapshot) or `XREAD` (tail).
#[derive(Debug, Clone, Default)]
pub struct RedisStreamEvent {
    /// The source stream key.
    pub key: String,
    /// The Redis-assigned entry ID, e.g. `"1526919030474-0"`.
    pub id: String,
    /// The entry's field → value pairs.
    pub fields: Vec<(String, Vec<u8>)>,
}

impl RedisStreamEvent {
    /// Look up a field's value by name.
    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_slice())
    }
}

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

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Subscribe to a Redis `channel` and stream every message published to it as a
/// [`RedisEvent`], as a `Burst<RedisEvent>` source.
///
/// Connects and issues `SUBSCRIBE` once at startup, then emits each incoming
/// message stamped with `NanoTime::now()`. Redis Pub/Sub has no backlog: only
/// messages published *after* the subscription is registered are delivered. Any
/// connection or subscribe error aborts the run with context. Use `.collapse()`
/// for single-event processing.
///
/// `run_mode` is used only to reject a historical run at wiring; the producer's
/// full [`RunParams`] are derived from the actual run at start (see
/// [`produce_async`] and the module docs). The graph owns the tokio runtime.
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: Pub/Sub is a live, unbounded, wall-clock-stamped
/// stream with no historical timeline to replay, and the historical channel path
/// would block-collect it up front and deadlock at `start`. Run under
/// [`RunMode::RealTime`].
pub fn redis_sub(
    g: &GraphBuilder,
    run_mode: RunMode,
    conn: impl Into<RedisConnection>,
    channel: impl Into<String>,
) -> Result<Stream<Burst<RedisEvent>>> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "redis_sub: RunMode::HistoricalFrom is unsupported — Redis Pub/Sub is a \
             live, unbounded, wall-clock-stamped stream with no historical timeline \
             to replay; run redis_sub under RunMode::RealTime"
        );
    }
    let conn = conn.into();
    let channel = channel.into();
    produce_async(
        g,
        move |_p: RunParams| async move {
            let client = redis::Client::open(conn.url.as_str())
                .with_context(|| format!("redis_sub: opening client for {}", conn.redacted()))?;
            let mut pubsub = client
                .get_async_pubsub()
                .await
                .with_context(|| format!("redis_sub: connecting to {}", conn.redacted()))?;
            pubsub
                .subscribe(channel.as_str())
                .await
                .with_context(|| format!("redis_sub: subscribing to {channel:?}"))?;

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
                yield Err(anyhow::anyhow!(
                    "redis_sub: subscription stream closed unexpectedly"
                ));
            })
        },
        None,
    )
}

/// Read a Redis stream `key`: emit a snapshot of all existing entries, then tail
/// live appends as [`RedisStreamEvent`]s, as a `Burst<RedisStreamEvent>` source.
///
/// The snapshot is read with `XRANGE key - +` (its entries share one timestamp,
/// so they ride one atomic burst) and its last entry ID is captured; the tail
/// then issues `XREAD BLOCK 0 STREAMS key <last_id>`, which only returns entries
/// with an ID strictly greater than `last_id`. Because the tail reads from the
/// exact snapshot boundary, no entry is missed or duplicated in the handoff. Use
/// `.collapse()` for single-event processing.
///
/// `run_mode` is used only to reject a historical run at wiring; the producer's
/// full [`RunParams`] are derived from the actual run at start (see
/// [`produce_async`] and the module docs). The graph owns the tokio runtime.
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: the stream tail (`XREAD BLOCK 0`) is a live,
/// unbounded, wall-clock-stamped stream with no historical timeline to replay,
/// and the historical channel path would block-collect it up front and deadlock
/// at `start`. Run under [`RunMode::RealTime`].
pub fn redis_stream_read(
    g: &GraphBuilder,
    run_mode: RunMode,
    conn: impl Into<RedisConnection>,
    key: impl Into<String>,
) -> Result<Stream<Burst<RedisStreamEvent>>> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "redis_stream_read: RunMode::HistoricalFrom is unsupported — the stream \
             tail (XREAD BLOCK 0) is a live, unbounded, wall-clock-stamped stream \
             with no historical timeline to replay; run redis_stream_read under \
             RunMode::RealTime"
        );
    }
    let conn = conn.into();
    let key = key.into();
    produce_async(
        g,
        move |_p: RunParams| async move {
            let client = redis::Client::open(conn.url.as_str()).with_context(|| {
                format!("redis_stream_read: opening client for {}", conn.redacted())
            })?;
            let mut connection = client
                .get_multiplexed_async_connection()
                .await
                .with_context(|| format!("redis_stream_read: connecting to {}", conn.redacted()))?;

            // 1. Snapshot all existing entries and capture the last ID.
            let snapshot: StreamRangeReply = connection
                .xrange(&key, "-", "+")
                .await
                .with_context(|| format!("redis_stream_read: XRANGE on {key:?} failed"))?;
            // "0" means "from the beginning" — used when the stream is empty so the
            // tail picks up the very first appended entry.
            let mut last_id = snapshot
                .ids
                .last()
                .map(|id| id.id.clone())
                .unwrap_or_else(|| "0".to_string());
            let snapshot_events: Vec<RedisStreamEvent> =
                snapshot.ids.iter().map(|id| to_event(&key, id)).collect();

            Ok(async_stream::stream! {
                // Phase 1: emit the snapshot as one atomic burst — every entry
                // shares ONE timestamp so a bounded run cannot observe only the
                // first (the etcd snapshot-burst reasoning).
                let snapshot_time = NanoTime::now();
                for event in snapshot_events {
                    yield Ok((snapshot_time, event));
                }

                // Phase 2: tail live appends from the snapshot boundary.
                let opts = StreamReadOptions::default().block(0);
                loop {
                    let reply: redis::RedisResult<StreamReadReply> =
                        connection.xread_options(&[&key], &[&last_id], &opts).await;
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
                            yield Err(anyhow::Error::new(e)
                                .context(format!("redis_stream_read: XREAD on {key:?} failed")));
                            break;
                        }
                    }
                }
            })
        },
        None,
    )
}

// ---------------------------------------------------------------------------
// Sinks
// ---------------------------------------------------------------------------

/// Extension trait providing a fluent API for publishing streams to Redis
/// Pub/Sub via `PUBLISH`.
///
/// Implemented for both `Stream<Burst<RedisEntry>>` (multi-item) and
/// `Stream<RedisEntry>` (single-item, auto-wrapped into one-element bursts), so
/// burst wrapping is never required in user code.
pub trait RedisSinkOps {
    /// Publish this stream to Redis via `PUBLISH`, issuing one `PUBLISH` per
    /// [`RedisEntry`] to the channel it names. Returns the sink `Stream<()>`.
    ///
    /// - `buffer_size`: back-pressure bound handed to
    ///   [`consume_async`](crate::async_source::consume_async) — `Some(n)` blocks
    ///   the graph once ~`n` entries are unwritten; `None` is unbounded.
    ///
    /// The graph owns the tokio runtime (see the module docs). Publishing runs
    /// off the graph thread; writes land in order and are flushed at teardown.
    /// `PUBLISH` returns the number of clients that received the message (which
    /// may be zero); this count is not surfaced.
    ///
    /// # Errors
    ///
    /// The connection is opened lazily on the first write, so a connection
    /// failure aborts the *run* (not wiring) with context; a bad URL still fails
    /// at wiring (`Client::open`). A per-entry publish failure aborts the run
    /// with context on a subsequent cycle.
    fn redis_pub(
        &self,
        conn: impl Into<RedisConnection>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>>;
}

impl RedisSinkOps for Stream<Burst<RedisEntry>> {
    fn redis_pub(
        &self,
        conn: impl Into<RedisConnection>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        let conn = conn.into();
        // The graph owns the runtime; the sink connects/publishes on it.
        let g = self.graph();
        // `Client::open` only parses the URL (no socket) — a config error, so it
        // stays at wiring. The socket opens lazily on the first write (below), so
        // wiring does no I/O and a connect failure aborts the run, not wiring.
        let client = redis::Client::open(conn.url.as_str())
            .with_context(|| format!("redis_pub: opening client for {}", conn.redacted()))?;
        let redacted = conn.redacted();
        let conn_cell: Arc<tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        let (sink, flush) = consume_async(&g, buffer_size, move |entry: RedisEntry| {
            let client = client.clone();
            let conn_cell = Arc::clone(&conn_cell);
            let redacted = redacted.clone();
            async move {
                let mut connection =
                    get_or_connect(&client, &conn_cell, &redacted, "redis_pub").await?;
                redis::cmd("PUBLISH")
                    .arg(entry.channel.as_str())
                    .arg(entry.payload.as_slice())
                    .query_async::<i64>(&mut connection)
                    .await
                    .with_context(|| format!("redis_pub: PUBLISH to {:?} failed", entry.channel))?;
                Ok(())
            }
        })?;
        Ok(self.for_each(sink).finally(flush))
    }
}

impl RedisSinkOps for Stream<RedisEntry> {
    fn redis_pub(
        &self,
        conn: impl Into<RedisConnection>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        self.map(|entry: &RedisEntry| burst![entry.clone()])
            .redis_pub(conn, buffer_size)
    }
}

/// Extension trait providing a fluent API for appending streams to Redis streams
/// via `XADD`.
///
/// Implemented for both `Stream<Burst<RedisStreamRecord>>` (multi-item) and
/// `Stream<RedisStreamRecord>` (single-item, auto-wrapped into one-element
/// bursts), so burst wrapping is never required in user code.
pub trait RedisStreamSinkOps {
    /// Append this stream to Redis streams via `XADD`, issuing one `XADD <key> *`
    /// per [`RedisStreamRecord`] (Redis assigns the entry ID). Returns the sink
    /// `Stream<()>`.
    ///
    /// - `buffer_size`: back-pressure bound handed to
    ///   [`consume_async`](crate::async_source::consume_async).
    ///
    /// The graph owns the tokio runtime (see the module docs). Appending runs off
    /// the graph thread; writes land in order and are flushed at teardown.
    ///
    /// # Errors
    ///
    /// The connection is opened lazily on the first write, so a connection
    /// failure aborts the *run* (not wiring) with context; a bad URL still fails
    /// at wiring (`Client::open`). A per-record append failure aborts the run
    /// with context on a subsequent cycle.
    fn redis_stream_write(
        &self,
        conn: impl Into<RedisConnection>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>>;
}

impl RedisStreamSinkOps for Stream<Burst<RedisStreamRecord>> {
    fn redis_stream_write(
        &self,
        conn: impl Into<RedisConnection>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        let conn = conn.into();
        // The graph owns the runtime; the sink connects/appends on it.
        let g = self.graph();
        // `Client::open` only parses the URL (no socket) — a config error, so it
        // stays at wiring. The socket opens lazily on the first write (below), so
        // wiring does no I/O and a connect failure aborts the run, not wiring.
        let client = redis::Client::open(conn.url.as_str()).with_context(|| {
            format!("redis_stream_write: opening client for {}", conn.redacted())
        })?;
        let redacted = conn.redacted();
        let conn_cell: Arc<tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        let (sink, flush) = consume_async(&g, buffer_size, move |record: RedisStreamRecord| {
            let client = client.clone();
            let conn_cell = Arc::clone(&conn_cell);
            let redacted = redacted.clone();
            async move {
                let mut connection =
                    get_or_connect(&client, &conn_cell, &redacted, "redis_stream_write").await?;
                let mut cmd = redis::cmd("XADD");
                cmd.arg(record.key.as_str()).arg("*");
                for (field, value) in &record.fields {
                    cmd.arg(field.as_str()).arg(value.as_slice());
                }
                cmd.query_async::<String>(&mut connection)
                    .await
                    .with_context(|| {
                        format!("redis_stream_write: XADD to {:?} failed", record.key)
                    })?;
                Ok(())
            }
        })?;
        Ok(self.for_each(sink).finally(flush))
    }
}

impl RedisStreamSinkOps for Stream<RedisStreamRecord> {
    fn redis_stream_write(
        &self,
        conn: impl Into<RedisConnection>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        self.map(|record: &RedisStreamRecord| burst![record.clone()])
            .redis_stream_write(conn, buffer_size)
    }
}

#[cfg(test)]
mod tests {
    use super::{RedisConnection, RedisEntry, RedisStreamEvent, RedisStreamRecord};

    #[test]
    fn entry_payload_str_reads_utf8() {
        let entry = RedisEntry {
            channel: "ch".to_string(),
            payload: b"bar".to_vec(),
        };
        assert_eq!(entry.payload_str().unwrap(), "bar");
    }

    #[test]
    fn stream_record_single_builds_one_field() {
        let record = RedisStreamRecord::single("k", "f", b"v".to_vec());
        assert_eq!(record.key, "k");
        assert_eq!(record.fields, vec![("f".to_string(), b"v".to_vec())]);
    }

    #[test]
    fn stream_event_field_lookup() {
        let event = RedisStreamEvent {
            key: "k".to_string(),
            id: "1-0".to_string(),
            fields: vec![
                ("a".to_string(), b"1".to_vec()),
                ("b".to_string(), b"2".to_vec()),
            ],
        };
        assert_eq!(event.field("a"), Some(b"1".as_ref()));
        assert_eq!(event.field("b"), Some(b"2".as_ref()));
        assert_eq!(event.field("missing"), None);
    }

    #[test]
    fn connection_from_str_and_string() {
        assert_eq!(
            RedisConnection::from("redis://h:6379").url,
            "redis://h:6379"
        );
        assert_eq!(
            RedisConnection::from("redis://h:6379".to_string()).url,
            "redis://h:6379"
        );
    }

    #[test]
    fn test_redacted_masks_user_and_password() {
        let conn = RedisConnection::new("redis://user:sup3rs3cr3t@127.0.0.1:6379/0");
        let out = conn.redacted();
        assert!(!out.contains("sup3rs3cr3t"), "password leaked: {out}");
        assert_eq!(out, "redis://***:***@127.0.0.1:6379/0");
    }

    #[test]
    fn test_redacted_masks_password_only_userinfo() {
        // `redis://:pass@host` — no username, just a password.
        let conn = RedisConnection::new("redis://:hunter2@host:6379");
        let out = conn.redacted();
        assert!(!out.contains("hunter2"), "password leaked: {out}");
        assert_eq!(out, "redis://***:***@host:6379");
    }

    #[test]
    fn test_redacted_masks_rediss_scheme() {
        let conn = RedisConnection::new("rediss://user:s3cr3t@host:6380/1");
        let out = conn.redacted();
        assert!(!out.contains("s3cr3t"), "password leaked: {out}");
        assert_eq!(out, "rediss://***:***@host:6380/1");
    }

    #[test]
    fn test_redacted_noop_without_userinfo() {
        let conn = RedisConnection::new("redis://127.0.0.1:6379/0");
        assert_eq!(conn.redacted(), "redis://127.0.0.1:6379/0");
    }
}
