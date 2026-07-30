//! fluvio adapter — a streaming topic-partition consume **source**
//! ([`fluvio_sub`]) and a topic-produce **sink**
//! ([`FluvioSinkOps::fluvio_pub`]) for [Fluvio](https://fluvio.io) clusters. It
//! ports the classic `wingfoil::adapters::fluvio` module onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`fluvio_sub`] on a
//!   [`GraphBuilder`]: streams records from one topic partition starting at a
//!   caller-chosen offset, emitting each record as a [`FluvioEvent`] in a
//!   `Stream<Burst<FluvioEvent>>`.
//! - **Sink** — the [`FluvioSinkOps`] extension trait on
//!   `Stream<Burst<FluvioRecord>>` (and, for convenience,
//!   `Stream<FluvioRecord>`), enabled with
//!   `use wingfoil_next::adapters::fluvio::FluvioSinkOps;`.
//!
//! Unlike [`etcd`](crate::adapters::etcd) there is **no snapshot + watch
//! duality** — Fluvio is a pure log, so `start_offset` alone selects between
//! "replay everything retained" and "tail from here".
//!
//! # Deviations from classic
//!
//! Every classic *capability* (offset-selected partition consumption, keyed and
//! keyless records, per-burst flush batching, the single-record convenience
//! sink) is preserved. The surface differs in these deliberate ways, mirroring
//! the [`kafka`](crate::adapters::kafka) port:
//!
//! 1. **The graph owns the tokio runtime.** Classic `fluvio_sub`/`fluvio_pub`
//!    hide a never-dropped global runtime inside `produce_async`/`consume_async`.
//!    Next's `GraphBuilder` owns one runtime, created lazily on first async use
//!    and dropped at teardown, shared by every async adapter — so the common call
//!    needs no `&Handle` and there is no leaked global (see
//!    `docs/runtime-ownership.md`). The consumer task spawns in `start()`,
//!    deferred via `produce_async`'s `source_at_start` wiring, so the cluster
//!    connection is established at run start rather than at wiring.
//! 2. **`fluvio_sub` is realtime-only, rejected at wiring time under historical
//!    replay.** The consumer is a live, unbounded, wall-clock-stamped stream: it
//!    replays the partition's retained records and then blocks for new ones
//!    forever, so the historical channel path (which block-collects the whole
//!    stream up front) would deadlock at `start`. Classic technically permitted a
//!    `HistoricalFrom` run (with wall-clock `NanoTime::now()` timestamps); next
//!    [rejects it at wiring time](fluvio_sub#errors) with a clear error rather
//!    than deadlocking. Run `fluvio_sub` under [`RunMode::RealTime`].
//! 3. **The sink is a trait only.** Classic exposed both a free `fluvio_pub`
//!    function and a `FluvioPubOperators` trait; next folds the single public
//!    entry point into the [`FluvioSinkOps`] trait (renamed for the
//!    sink-as-trait convention shared with [`lines`](crate::adapters::lines) /
//!    [`csv`](crate::adapters::csv) / [`kafka`](crate::adapters::kafka)).
//! 4. **The sink connects lazily, on the first burst.** Classic connected once
//!    inside its `consume_async` closure before draining the stream; next's
//!    consumer task does the same on its first burst, so wiring does no I/O and
//!    an unreachable cluster aborts the *run* (via `consume_async_bursts`'s error
//!    channel) rather than graph construction — in line with the defer-to-start
//!    direction (register A1/A4).
//! 5. **A negative `start_offset` is rejected at wiring.** Classic deferred the
//!    check into the producer future (so the error surfaced at run start); the
//!    validation is pure, so next fails fast at wiring instead.
//!
//! ## Runtime requirement (a `block_on` footgun)
//!
//! The sink drives its writes off the graph thread with
//! [`consume_async_bursts`](crate::async_source::consume_async_bursts), which
//! uses [`Handle::block_on`](tokio::runtime::Handle::block_on) on the graph
//! thread for back-pressure and the teardown flush. So **the graph must be
//! built, run, and dropped from a non-async thread** (`main`, a `#[test]` fn).
//!
//! # Offsets
//!
//! [`fluvio_sub`] consumes a single partition of a topic:
//!
//! - `start_offset: None` — from the beginning of the partition
//!   (`Offset::beginning()`), replaying every retained record.
//! - `start_offset: Some(n)` — from absolute offset `n`, inclusive, skipping
//!   everything before it.
//!
//! Fluvio has no consumer-group offset tracking in this adapter: each run starts
//! where `start_offset` says, so a resumable consumer records the last
//! [`FluvioEvent::offset`] it saw and passes `Some(offset + 1)` on the next run.
//!
//! # Sink
//!
//! [`FluvioSinkOps::fluvio_pub`] sends one record per [`FluvioRecord`] in each
//! burst, then issues a single `flush()` per burst — batching within a tick for
//! throughput while still guaranteeing delivery before the run moves on (classic
//! parity). Records with `key: None` are sent with [`RecordKey::NULL`]. A send or
//! flush failure aborts the run with context (on the next cycle, or via the
//! `flush` teardown for the final burst).
//!
//! # Setup
//!
//! A Fluvio cluster is not a single process: it needs an **SC** (System
//! Controller, port 9003) and at least one **SPU** (Stream Processing Unit, port
//! 9010). The simplest local setup is the Fluvio CLI:
//!
//! ```sh
//! fluvio cluster start --local
//! fluvio topic create my-topic
//! ```
//!
//! Topics must exist before a producer or consumer connects — Fluvio returns
//! `TopicNotFound` otherwise.

use std::sync::Arc;

use anyhow::{Context, Result};
use fluvio::consumer::ConsumerConfigExt;
use fluvio::{Fluvio, FluvioClusterConfig, Offset, RecordKey, TopicProducerPool};
use futures::StreamExt;
use wingfoil::{NanoTime, RunMode};

use crate::Burst;
use crate::async_source::{RunParams, consume_async_bursts, produce_async};
use crate::burst;
use crate::fluent::{GraphBuilder, Stream, StreamOps};

/// Connection configuration for a Fluvio cluster.
#[derive(Debug, Clone)]
pub struct FluvioConnection {
    /// Fluvio SC (System Controller) endpoint in `host:port` format.
    ///
    /// The default Fluvio SC port is 9003. Do **not** include a URL scheme
    /// (`http://` or `https://`) — Fluvio uses a custom binary protocol.
    ///
    /// Example: `"127.0.0.1:9003"`
    pub endpoint: String,
}

impl FluvioConnection {
    /// Create a connection config with the given SC endpoint.
    ///
    /// The endpoint should be in `host:port` format, e.g. `"127.0.0.1:9003"`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

impl From<&str> for FluvioConnection {
    fn from(endpoint: &str) -> Self {
        Self::new(endpoint)
    }
}

impl From<String> for FluvioConnection {
    fn from(endpoint: String) -> Self {
        Self::new(endpoint)
    }
}

impl From<&String> for FluvioConnection {
    fn from(endpoint: &String) -> Self {
        Self::new(endpoint.clone())
    }
}

/// A record to write to a Fluvio topic.
#[derive(Debug, Clone, Default)]
pub struct FluvioRecord {
    /// Optional record key. `None` sends the record without a key
    /// ([`RecordKey::NULL`]).
    pub key: Option<String>,
    /// Record payload as raw bytes.
    pub value: Vec<u8>,
}

impl FluvioRecord {
    /// Create a keyless record with the given value.
    pub fn new(value: Vec<u8>) -> Self {
        Self { key: None, value }
    }

    /// Create a record with both key and value.
    pub fn with_key(key: impl Into<String>, value: Vec<u8>) -> Self {
        Self {
            key: Some(key.into()),
            value,
        }
    }
}

/// A record received from a Fluvio topic.
#[derive(Debug, Clone, Default)]
pub struct FluvioEvent {
    /// Record key, if present. `None` for keyless records.
    pub key: Option<Vec<u8>>,
    /// Record payload as raw bytes.
    pub value: Vec<u8>,
    /// Absolute partition offset of this record.
    pub offset: i64,
}

impl FluvioEvent {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }

    /// Interpret the key as a UTF-8 string, if present.
    pub fn key_str(&self) -> Option<std::result::Result<&str, std::str::Utf8Error>> {
        self.key.as_deref().map(std::str::from_utf8)
    }
}

/// Stream records from a Fluvio topic partition as a `Burst<FluvioEvent>`
/// source.
///
/// Connects to the Fluvio SC at `conn.endpoint` and continuously delivers
/// records from `topic` / `partition` as [`FluvioEvent`]s. Records arriving
/// between graph cycles group into one [`Burst`]; use `.collapse_accumulate()`
/// (or `.collapse()`) for single-event processing.
///
/// `run_mode` is the mode the graph will be driven with — used only to reject a
/// historical run at wiring (see below); the producer's full [`RunParams`] are
/// derived from the actual run at start (see [`produce_async`] and the module
/// docs). The graph owns the tokio runtime.
///
/// # Arguments
///
/// - `conn` — Fluvio cluster configuration (SC endpoint).
/// - `topic` — the Fluvio topic to consume from. It must already exist.
/// - `partition` — partition index (0-based). Use `0` for single-partition
///   topics.
/// - `start_offset` — where to begin consuming: `None` from the beginning of the
///   partition, `Some(n)` from absolute offset `n` (inclusive).
///
/// # Errors
///
/// Returns an error at **wiring time** if:
///
/// - `run_mode` is [`RunMode::HistoricalFrom`]: the Fluvio consumer is a live,
///   unbounded, wall-clock-stamped stream with no historical timeline to replay,
///   and the historical channel path would block-collect its never-ending stream
///   up front and deadlock at `start`. Run `fluvio_sub` under
///   [`RunMode::RealTime`].
/// - `start_offset` is negative.
///
/// The run aborts with context if the connection to the SC fails, the consumer
/// cannot be created (e.g. the topic does not exist), or the cluster returns a
/// record-level error.
pub fn fluvio_sub(
    g: &GraphBuilder,
    run_mode: RunMode,
    conn: impl Into<FluvioConnection>,
    topic: impl Into<String>,
    partition: u32,
    start_offset: Option<i64>,
) -> Result<Stream<Burst<FluvioEvent>>> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "fluvio_sub: RunMode::HistoricalFrom is unsupported — the Fluvio consumer \
             is a live, unbounded, wall-clock-stamped stream with no historical \
             timeline to replay; run fluvio_sub under RunMode::RealTime"
        );
    }
    // Pure validation, so it fails fast at wiring (classic deferred it into the
    // producer future — see deviation 5 in the module docs).
    if let Some(n) = start_offset
        && n < 0
    {
        anyhow::bail!("fluvio_sub: start_offset must be non-negative, got {n}");
    }

    let conn = conn.into();
    let topic = topic.into();
    produce_async(
        g,
        move |_p: RunParams| async move {
            let cluster_config = FluvioClusterConfig::new(&conn.endpoint);
            let client = Fluvio::connect_with_config(&cluster_config)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("fluvio_sub: connecting to {} failed: {e}", conn.endpoint)
                })?;

            let offset = match start_offset {
                None => Offset::beginning(),
                Some(n) => Offset::absolute(n)
                    .map_err(|e| anyhow::anyhow!("fluvio_sub: invalid offset {n}: {e}"))?,
            };

            let consumer_config = ConsumerConfigExt::builder()
                .topic(topic.clone())
                .partition(partition)
                .offset_start(offset)
                .build()
                .map_err(|e| anyhow::anyhow!("fluvio_sub: consumer config failed: {e}"))?;

            let mut stream = client
                .consumer_with_config(consumer_config)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("fluvio_sub: creating consumer for {topic} failed: {e}")
                })?;

            Ok(async_stream::stream! {
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(record) => {
                            let event = FluvioEvent {
                                key: record.key().map(|k| k.to_vec()),
                                value: record.value().to_vec(),
                                offset: record.offset,
                            };
                            yield Ok((NanoTime::now(), event));
                        }
                        Err(e) => {
                            // `ErrorCode` does not implement `std::error::Error`,
                            // so it is rendered with Debug rather than Display.
                            yield Err(anyhow::anyhow!("fluvio_sub: record error: {e:?}"));
                            break;
                        }
                    }
                }
            })
        },
        None,
    )
}

/// Extension trait providing a fluent API for producing streams to Fluvio.
///
/// Implemented for both `Stream<Burst<FluvioRecord>>` (multi-item) and
/// `Stream<FluvioRecord>` (single-item, auto-wrapped into one-element bursts),
/// so burst wrapping is never required in user code.
pub trait FluvioSinkOps {
    /// Write this stream's [`FluvioRecord`]s to a Fluvio `topic`. Returns the
    /// sink `Stream<()>`.
    ///
    /// The graph owns the tokio runtime (see the module docs). Records are
    /// drained off the graph thread one burst at a time: every record in a burst
    /// is sent, then a single `flush()` guarantees delivery before the next
    /// burst — batching within a tick for throughput (classic parity). Bursts
    /// are written in order.
    ///
    /// - `conn` — Fluvio cluster configuration (SC endpoint).
    /// - `topic` — the Fluvio topic to produce to. It must already exist.
    /// - `buffer_size` — back-pressure bound handed to
    ///   [`consume_async_bursts`](crate::async_source::consume_async_bursts);
    ///   `None` is unbounded.
    ///
    /// # Errors
    ///
    /// The cluster connection and the topic producer are established lazily on
    /// the first burst, so an unreachable SC or a missing topic aborts the *run*
    /// (not wiring) with context. A send or flush failure aborts the run with
    /// context on a subsequent cycle, or via the teardown flush for the final
    /// burst. Wiring only fails if the graph's tokio runtime cannot be created.
    fn fluvio_pub(
        &self,
        conn: impl Into<FluvioConnection>,
        topic: impl Into<String>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>>;
}

impl FluvioSinkOps for Stream<Burst<FluvioRecord>> {
    fn fluvio_pub(
        &self,
        conn: impl Into<FluvioConnection>,
        topic: impl Into<String>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        let conn = conn.into();
        let topic = topic.into();
        let g = self.graph();
        // The producer is built on the first burst and reused thereafter. The
        // mutex is only ever taken inside the consumer task (off the graph
        // thread), so the no-locks-on-the-graph-path invariant holds.
        let producer: Arc<tokio::sync::Mutex<Option<TopicProducerPool>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        // `consume_async_bursts` hands the consumer one whole burst at a time, so
        // the per-burst flush below batches exactly the records classic batched.
        let (sink, flush) =
            consume_async_bursts(&g, buffer_size, move |records: Vec<FluvioRecord>| {
                let producer = Arc::clone(&producer);
                let conn = conn.clone();
                let topic = topic.clone();
                async move {
                    let mut slot = producer.lock().await;
                    if slot.is_none() {
                        let cluster_config = FluvioClusterConfig::new(&conn.endpoint);
                        let client =
                            Fluvio::connect_with_config(&cluster_config)
                                .await
                                .map_err(|e| {
                                    anyhow::anyhow!(
                                        "fluvio_pub: connecting to {} failed: {e}",
                                        conn.endpoint
                                    )
                                })?;
                        let created = client.topic_producer(&topic).await.map_err(|e| {
                            anyhow::anyhow!("fluvio_pub: creating producer for {topic} failed: {e}")
                        })?;
                        *slot = Some(created);
                    }
                    let producer = slot
                        .as_ref()
                        .context("invariant: fluvio producer created above")?;

                    for record in records {
                        let key = record.key.map(RecordKey::from).unwrap_or(RecordKey::NULL);
                        producer.send(key, record.value).await.map_err(|e| {
                            anyhow::anyhow!("fluvio_pub: sending to {topic} failed: {e}")
                        })?;
                    }
                    // Flush once per burst to batch records within a tick for
                    // throughput while still guaranteeing delivery before the
                    // next cycle's records are sent.
                    producer
                        .flush()
                        .await
                        .map_err(|e| anyhow::anyhow!("fluvio_pub: flushing {topic} failed: {e}"))?;
                    Ok(())
                }
            })?;
        Ok(self.for_each(sink).finally(flush))
    }
}

impl FluvioSinkOps for Stream<FluvioRecord> {
    fn fluvio_pub(
        &self,
        conn: impl Into<FluvioConnection>,
        topic: impl Into<String>,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        self.map(|record: &FluvioRecord| burst![record.clone()])
            .fluvio_pub(conn, topic, buffer_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_constructors_set_key() {
        let keyless = FluvioRecord::new(b"v".to_vec());
        assert_eq!(keyless.key, None);
        assert_eq!(keyless.value, b"v");

        let keyed = FluvioRecord::with_key("k", b"v".to_vec());
        assert_eq!(keyed.key.as_deref(), Some("k"));
    }

    #[test]
    fn event_value_str_reads_utf8() {
        let event = FluvioEvent {
            key: Some(b"field-key".to_vec()),
            value: b"field-value".to_vec(),
            offset: 7,
        };
        assert_eq!(event.value_str().unwrap(), "field-value");
        assert_eq!(event.key_str().unwrap().unwrap(), "field-key");
    }

    #[test]
    fn event_no_key_is_none() {
        let event = FluvioEvent {
            key: None,
            value: b"val".to_vec(),
            offset: 0,
        };
        assert!(event.key_str().is_none());
    }

    #[test]
    fn connection_from_str() {
        let conn = FluvioConnection::from("127.0.0.1:9003");
        assert_eq!(conn.endpoint, "127.0.0.1:9003");
    }
}
