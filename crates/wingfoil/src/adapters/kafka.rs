//! kafka adapter — a streaming topic-consume **source** (`kafka_sub`) and a
//! topic-produce **sink** (`KafkaSinkOps::kafka_pub`) for Apache Kafka. It ports
//! the legacy `wingfoil::adapters::kafka` module onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`kafka_sub`] on a
//!   [`GraphBuilder`]: consumes a topic in a consumer group and emits each
//!   message as a [`KafkaEvent`], as a `Stream<Burst<KafkaEvent>>`.
//! - **Mode-agnostic source** — [`kafka_source`] with a [`KafkaSourceConfig`],
//!   which dispatches on the run's [`RunMode`] at wiring so the mode choice stays
//!   at `run()` rather than in the function name. Only the live half exists today
//!   (a bounded offset-range replay reader is not implemented — deviation
//!   register B2), so a historical run errors at wiring; prefer it over
//!   [`kafka_sub`] at call sites that may want to flip modes later.
//! - **Sink** — the [`KafkaSinkOps`] extension trait on `Stream<Burst<KafkaRecord>>`
//!   (and, for convenience, `Stream<KafkaRecord>`), enabled with
//!   `use wingfoil::adapters::kafka::KafkaSinkOps;`.
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* (consumer-group offset tracking, the earliest
//! auto-offset-reset, per-record topic/key/partition, at-most-once delivery via
//! background auto-commit, multi-topic writes from one sink) is preserved. The
//! surface differs in these deliberate ways:
//!
//! 1. **The graph owns the tokio runtime.** Legacy `kafka_sub`/`kafka_pub` hide
//!    a never-dropped global runtime inside `produce_async`/`consume_async`.
//!    Wingfoil's `GraphBuilder` owns one runtime, created lazily on first async use
//!    and dropped at teardown, shared by every async adapter — so the common call
//!    needs no `&Handle` and there is no leaked global (see
//!    `docs/runtime-ownership.md`). `kafka_sub` takes a [`RunMode`] (only to reject
//!    a historical run at wiring); the producer task spawns in `start()`, deferred
//!    via `source_at_start`, so the broker consumer starts at run start, not at
//!    wiring, and the producer's `RunParams` come from the actual run. Embed in an
//!    existing runtime by installing an override with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime).
//! 2. **`kafka_sub` is realtime-only, rejected at wiring time under historical
//!    replay.** The consumer is a live, unbounded, wall-clock-stamped stream
//!    with no historical timeline to replay: its `recv()` loop never ends, so
//!    the historical channel path (which block-collects the whole stream up
//!    front) would deadlock at `start`. Legacy technically permitted a
//!    `HistoricalFrom` run (with wall-clock `NanoTime::now()` timestamps); next
//!    [rejects it at wiring time](kafka_sub#errors) with a clear error rather
//!    than deadlocking. Run `kafka_sub` under [`RunMode::RealTime`].
//! 3. **The sink is a trait only.** Legacy exposed both a free `kafka_pub`
//!    function and a `KafkaPubOperators` trait; wingfoil folds the single public
//!    entry point into the [`KafkaSinkOps`] trait (renamed for the
//!    sink-as-trait convention shared with [`lines`](crate::adapters::lines) /
//!    [`csv`](crate::adapters::csv) / [`etcd`](crate::adapters::etcd)). The
//!    [`FutureProducer`] is created at wiring time (a `ClientConfig::create()`
//!    config check — no socket — and the factory returns [`Result`]), but
//!    librdkafka connects **lazily on the first `send()`**, so wiring opens no
//!    broker connection and a bad broker surfaces as a produce failure during
//!    the run rather than at wiring — in line with the defer-to-start direction
//!    (register A1), no migration needed.
//!
//! Records are produced **concurrently per burst** — at parity with legacy: the
//! sink rides [`consume_async_bursts`](crate::async_source::consume_async_bursts)
//! and hands a whole burst's records to the producer up front, draining their
//! delivery futures together via `FuturesUnordered`, so per-burst latency is
//! ~one broker roundtrip rather than N. Order is preserved *across* bursts (the
//! single consumer awaits each burst to completion before the next). The explicit
//! `producer.flush()` legacy did at upstream end is unnecessary here: every send
//! is awaited to its delivery ack (nothing is left queued), and the consumer
//! drains all queued bursts at teardown.
//!
//! # Consumer group and offsets
//!
//! [`kafka_sub`] creates a `StreamConsumer` in the given `group_id` and
//! subscribes to the topic. The `group_id` determines offset tracking: the same
//! group across multiple runs continues from the last committed offset; a unique
//! group reads from the beginning (`auto.offset.reset = earliest`). Delivery is
//! **at-most-once** — `enable.auto.commit = true` commits offsets periodically
//! in the background, so a message fetched but not fully processed before an
//! error is not re-delivered. For at-least-once, disable auto-commit and manage
//! offsets via the rdkafka API.
//!
//! # Sink
//!
//! [`KafkaSinkOps::kafka_pub`] produces one Kafka message per [`KafkaRecord`] in
//! each burst, all records in a burst concurrently (one broker roundtrip per
//! burst). Each record names its own target topic, so a single sink can write to
//! many topics. A delivery failure aborts the run with context (on the next
//! cycle, or via the `flush` teardown for the final burst, per
//! [`consume_async_bursts`](crate::async_source::consume_async_bursts)).
//!
//! # Setup
//!
//! Using Redpanda (Kafka-compatible, faster startup):
//!
//! ```sh
//! docker run --rm -p 9092:9092 \
//!   docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
//!   redpanda start --overprovisioned --smp 1 --memory 512M \
//!   --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
//! ```

use anyhow::{Context, Result};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use rdkafka::Message;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::error::KafkaError;
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::types::RDKafkaErrorCode;
use std::time::Duration;
use wingfoil::{NanoTime, RunMode};

use crate::Burst;
use crate::async_source::{RunParams, consume_async_bursts, produce_async};
use crate::burst;
use crate::fluent::{GraphBuilder, Stream, StreamOps};

/// Delivery-confirmation timeout for a single produced record.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Connection configuration for Kafka.
#[derive(Debug, Clone)]
pub struct KafkaConnection {
    /// Kafka bootstrap servers, e.g. `"localhost:9092"`.
    ///
    /// Multiple brokers can be comma-separated: `"host1:9092,host2:9092"`.
    pub brokers: String,
}

impl KafkaConnection {
    /// Create a connection config with a broker string.
    ///
    /// Multiple brokers can be separated by commas: `"host1:9092,host2:9092"`.
    pub fn new(brokers: impl Into<String>) -> Self {
        Self {
            brokers: brokers.into(),
        }
    }
}

impl From<&str> for KafkaConnection {
    fn from(brokers: &str) -> Self {
        Self::new(brokers)
    }
}

impl From<String> for KafkaConnection {
    fn from(brokers: String) -> Self {
        Self::new(brokers)
    }
}

impl From<&String> for KafkaConnection {
    fn from(brokers: &String) -> Self {
        Self::new(brokers.clone())
    }
}

/// A record to be produced to Kafka.
#[derive(Debug, Clone, Default)]
pub struct KafkaRecord {
    /// The target topic.
    pub topic: String,
    /// Optional message key (used for partitioning).
    pub key: Option<Vec<u8>>,
    /// The message payload.
    pub value: Vec<u8>,
}

impl KafkaRecord {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }
}

/// An event consumed from a Kafka topic.
#[derive(Debug, Clone, Default)]
pub struct KafkaEvent {
    /// The source topic.
    pub topic: String,
    /// The partition from which this message was consumed.
    pub partition: i32,
    /// The offset of this message within the partition.
    pub offset: i64,
    /// The message key, if present.
    pub key: Option<Vec<u8>>,
    /// The message payload.
    pub value: Vec<u8>,
}

impl KafkaEvent {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }

    /// Interpret the key as a UTF-8 string, if present.
    pub fn key_str(&self) -> Option<std::result::Result<&str, std::str::Utf8Error>> {
        self.key.as_deref().map(std::str::from_utf8)
    }
}

/// Consume messages from a Kafka `topic` in the `group_id` consumer group as a
/// `Burst<KafkaEvent>` source.
///
/// Creates a `StreamConsumer` in the given group, subscribes to the topic, and
/// emits each message as a [`KafkaEvent`] carrying the topic, partition, offset,
/// key, and value. Messages arriving between graph cycles group into one
/// [`Burst`]; iterate the burst to process every message. (`.collapse()` keeps
/// only the burst's **last** message and silently drops the rest — reach for
/// it only for a latest-wins signal; see [`Collapse`](crate::ops::Collapse).)
///
/// `run_mode` is the mode the graph will be driven with — used only to reject a
/// historical run at wiring (see below); the producer's full [`RunParams`] are
/// derived from the actual run at start (see [`produce_async`] and the module
/// docs). The graph owns the tokio runtime.
///
/// # Consumer group and delivery
///
/// The same `group_id` across runs continues from the last committed offset; a
/// unique group reads from the beginning (`auto.offset.reset = earliest`).
/// Delivery is at-most-once (`enable.auto.commit = true`); see the module docs.
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: the Kafka consumer is a live, unbounded,
/// wall-clock-stamped stream with no historical timeline to replay, and the
/// historical channel path would block-collect its never-ending `recv()` loop up
/// front and deadlock at `start`. Run `kafka_sub` under [`RunMode::RealTime`].
pub fn kafka_sub(
    g: &GraphBuilder,
    run_mode: RunMode,
    conn: impl Into<KafkaConnection>,
    topic: impl Into<String>,
    group_id: impl Into<String>,
) -> Result<Stream<Burst<KafkaEvent>>> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "kafka_sub: RunMode::HistoricalFrom is unsupported — the Kafka consumer \
             is a live, unbounded, wall-clock-stamped stream with no historical \
             timeline to replay; run kafka_sub under RunMode::RealTime"
        );
    }
    let conn = conn.into();
    let topic = topic.into();
    let group_id = group_id.into();
    produce_async(
        g,
        move |_p: RunParams| async move {
            let consumer: StreamConsumer = ClientConfig::new()
                .set("bootstrap.servers", &conn.brokers)
                .set("group.id", &group_id)
                .set("auto.offset.reset", "earliest")
                .set("enable.auto.commit", "true")
                .set("session.timeout.ms", "6000")
                .create()
                .context("kafka_sub: creating consumer")?;

            consumer
                .subscribe(&[&topic])
                .with_context(|| format!("kafka_sub: subscribing to {topic}"))?;

            Ok(async_stream::stream! {
                loop {
                    match consumer.recv().await {
                        Ok(msg) => {
                            let event = KafkaEvent {
                                topic: msg.topic().to_string(),
                                partition: msg.partition(),
                                offset: msg.offset(),
                                key: msg.key().map(|k| k.to_vec()),
                                value: msg.payload().unwrap_or_default().to_vec(),
                            };
                            yield Ok((NanoTime::now(), event));
                        }
                        Err(e) if is_transient_subscribe_error(&e) => continue,
                        Err(e) => {
                            yield Err(anyhow::anyhow!("kafka_sub: consume error: {e}"));
                            break;
                        }
                    }
                }
            })
        },
        None,
    )
}

/// `UnknownTopicOrPartition` is reported while the broker is still catching up on
/// topic metadata (or the topic is pending auto-create). librdkafka keeps
/// retrying the subscription in the background, so we swallow it and wait for the
/// next event rather than terminating the stream.
fn is_transient_subscribe_error(err: &KafkaError) -> bool {
    matches!(
        err,
        KafkaError::MessageConsumption(RDKafkaErrorCode::UnknownTopicOrPartition)
    )
}

// ---------------------------------------------------------------------------
// Mode-agnostic source
// ---------------------------------------------------------------------------

/// The per-mode specs for [`kafka_source`], each optional.
///
/// [`RunMode`] is a *run-time* choice — build the graph once, pick real-time vs
/// historical at `run()` — but a mode-locked source function forces that choice
/// into *wiring*. This config carries one spec per mode so a single
/// [`kafka_source`] call can dispatch on the run's mode at wiring, exactly as
/// [`postgres_source`](crate::adapters::postgres::postgres_source) does.
///
/// Kafka has **only a live half today**: the durable log makes a bounded
/// offset-range replay feasible, but no such reader is implemented (deviation
/// register B2 — it needs an offsets-for-times window lookup, explicit partition
/// assignment instead of consumer-group subscription, a record timestamp on
/// [`KafkaEvent`], and a timestamp-ordered merge across partitions). A
/// `HistoricalFrom` run therefore errors at wiring, naming the unimplemented half
/// rather than a config option that does not exist. When the bounded reader lands
/// it becomes a `.historical(..)` builder here and existing call sites keep
/// working unchanged.
///
/// `group_id` belongs to the live half specifically, not to the source as a
/// whole: a bounded replay must *assign* explicit partitions rather than join a
/// consumer group, so it would carry no `group_id` at all.
#[derive(Default)]
pub struct KafkaSourceConfig {
    live: Option<LiveSpec>,
}

/// The live half — see [`KafkaSourceConfig::live`].
struct LiveSpec {
    group_id: String,
}

impl KafkaSourceConfig {
    /// An empty config — add the live half with [`live`](Self::live).
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the **live** (consumer-group tail) half — the [`kafka_sub`]
    /// mechanism. `group_id` is the consumer group whose committed offsets track
    /// consumption progress.
    pub fn live(mut self, group_id: impl Into<String>) -> Self {
        self.live = Some(LiveSpec {
            group_id: group_id.into(),
        });
        self
    }
}

/// A **mode-agnostic** Kafka source: dispatches on the run's [`RunMode`] at
/// wiring time to the mechanism that mode needs, per the [`KafkaSourceConfig`]
/// halves.
///
/// - [`RunMode::RealTime`] → the live consumer-group tail ([`kafka_sub`]), which
///   requires [`KafkaSourceConfig::live`].
/// - [`RunMode::HistoricalFrom`] → **unsupported**: kafka has no bounded
///   historical reader yet (see [`KafkaSourceConfig`]).
///
/// Wiring through `_source` rather than [`kafka_sub`] directly costs nothing
/// today and means the call site does not change when the historical half lands
/// — the mode choice already lives at `run()`, not in the function name.
/// [`kafka_sub`] remains the low-level primitive; this wires through it. See the
/// deviation register's "B2 — agreed plan: unified `<adapter>_source`".
///
/// `params` is the run the graph will be driven with; only
/// [`run_mode`](RunParams::run_mode) is read today, but taking the full
/// [`RunParams`] means the signature is already the one a bounded historical
/// reader needs (it slices the run's `[start, end)` window), matching
/// [`postgres_source`](crate::adapters::postgres::postgres_source).
///
/// # Errors
///
/// Returns an error at **wiring time** if the run mode is
/// [`RunMode::HistoricalFrom`], or if a [`RunMode::RealTime`] run has no live
/// half configured (the message names the missing half).
pub fn kafka_source(
    g: &GraphBuilder,
    params: RunParams,
    conn: impl Into<KafkaConnection>,
    topic: impl Into<String>,
    cfg: KafkaSourceConfig,
) -> Result<Stream<Burst<KafkaEvent>>> {
    match params.run_mode {
        RunMode::HistoricalFrom(_) => anyhow::bail!(
            "kafka_source: run mode is RunMode::HistoricalFrom, but the kafka adapter has no \
             historical half — a bounded offset-range replay reader is not implemented, and the \
             live consumer is an unbounded, wall-clock-stamped tail with no historical timeline \
             to replay; run under RunMode::RealTime with a .live(..) config"
        ),
        RunMode::RealTime => {
            let live = cfg.live.ok_or_else(|| {
                anyhow::anyhow!(
                    "kafka_source: run mode is RunMode::RealTime but no live config was \
                     supplied — add .live(group_id)"
                )
            })?;
            kafka_sub(g, params.run_mode, conn, topic, live.group_id)
        }
    }
}

/// Extension trait providing a fluent API for producing streams to Kafka.
///
/// Implemented for both `Stream<Burst<KafkaRecord>>` (multi-item) and
/// `Stream<KafkaRecord>` (single-item, auto-wrapped into one-element bursts), so
/// burst wrapping is never required in user code.
pub trait KafkaSinkOps {
    /// Produce this stream's [`KafkaRecord`]s to Kafka. Each record names its own
    /// target topic, so one sink can write to many topics. Returns the sink
    /// `Stream<()>`.
    ///
    /// The graph owns the tokio runtime (see the module docs).
    ///
    /// Records are drained off the graph thread one burst at a time; within a
    /// burst all records are produced concurrently (one broker roundtrip), and
    /// bursts are produced in order. A delivery failure aborts the run with
    /// context (surfaced on the next cycle, or via the `flush` teardown for the
    /// final burst, per [`consume_async_bursts`](crate::async_source::consume_async_bursts)).
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time if the [`FutureProducer`] cannot be
    /// created (a client-config error). librdkafka connects lazily, so an
    /// unreachable broker surfaces as a produce failure during the run, not at
    /// wiring.
    fn kafka_pub(&self, conn: impl Into<KafkaConnection>) -> Result<Stream<()>>;
}

impl KafkaSinkOps for Stream<Burst<KafkaRecord>> {
    fn kafka_pub(&self, conn: impl Into<KafkaConnection>) -> Result<Stream<()>> {
        let conn = conn.into();
        // Create the producer at wiring time so a config error surfaces before
        // the run (librdkafka connects lazily; a bad broker surfaces on the
        // first produced record).
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", &conn.brokers)
            .set("message.timeout.ms", "5000")
            .create()
            .context("kafka_pub: creating producer")?;

        // `consume_async_bursts` drains one whole burst at a time through a single
        // consumer task off the graph thread (order preserved *across* bursts); a
        // write error propagates back into the graph on the next cycle, and the
        // final burst's error is surfaced by the `flush` teardown wired below. The
        // producer is cloned per burst (it is an `Arc` internally, cheap to clone
        // and `Send`). The graph owns the runtime the consumer task runs on.
        let (sink, flush) =
            consume_async_bursts(&self.graph(), None, move |records: Vec<KafkaRecord>| {
                let producer = producer.clone();
                async move {
                    // Build every record's delivery future up front and drive them
                    // together via `FuturesUnordered`, so the whole burst is in
                    // flight at once (~one broker roundtrip, matching legacy)
                    // rather than N sequential roundtrips. Order within a burst is
                    // not observable to downstream (the sink emits `()`), so
                    // draining out of completion order is fine.
                    let mut inflight = FuturesUnordered::new();
                    for record in &records {
                        let mut fut_record = FutureRecord::to(&record.topic).payload(&record.value);
                        if let Some(ref key) = record.key {
                            fut_record = fut_record.key(key.as_slice());
                        }
                        let topic = record.topic.clone();
                        let delivery = producer.send(fut_record, SEND_TIMEOUT);
                        inflight.push(async move {
                            delivery.await.map_err(|(e, _)| {
                                anyhow::anyhow!("kafka_pub: producing to {topic} failed: {e}")
                            })
                        });
                    }
                    while let Some(result) = inflight.next().await {
                        result?;
                    }
                    Ok(())
                }
            })?;
        Ok(self.for_each(sink).finally(flush))
    }
}

impl KafkaSinkOps for Stream<KafkaRecord> {
    fn kafka_pub(&self, conn: impl Into<KafkaConnection>) -> Result<Stream<()>> {
        self.map(|record: &KafkaRecord| burst![record.clone()])
            .kafka_pub(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_value_str_reads_utf8() {
        let record = KafkaRecord {
            topic: "t".to_string(),
            key: Some(b"k".to_vec()),
            value: b"hello".to_vec(),
        };
        assert_eq!(record.value_str().unwrap(), "hello");
    }

    #[test]
    fn event_no_key_is_none() {
        let event = KafkaEvent {
            topic: "t".to_string(),
            partition: 0,
            offset: 0,
            key: None,
            value: b"val".to_vec(),
        };
        assert!(event.key_str().is_none());
    }

    #[test]
    fn event_key_and_value_str() {
        let event = KafkaEvent {
            topic: "t".to_string(),
            partition: 1,
            offset: 7,
            key: Some(b"field-key".to_vec()),
            value: b"field-value".to_vec(),
        };
        assert_eq!(event.value_str().unwrap(), "field-value");
        assert_eq!(event.key_str().unwrap().unwrap(), "field-key");
    }

    #[test]
    fn connection_from_str() {
        let conn = KafkaConnection::from("localhost:9092");
        assert_eq!(conn.brokers, "localhost:9092");
    }

    #[test]
    fn transient_subscribe_error_detected() {
        let transient = KafkaError::MessageConsumption(RDKafkaErrorCode::UnknownTopicOrPartition);
        assert!(is_transient_subscribe_error(&transient));

        let fatal = KafkaError::MessageConsumption(RDKafkaErrorCode::BrokerNotAvailable);
        assert!(!is_transient_subscribe_error(&fatal));
    }
}
