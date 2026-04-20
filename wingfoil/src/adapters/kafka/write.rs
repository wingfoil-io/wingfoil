//! Kafka producer consumer — writes messages to Kafka topics.

use super::{KafkaConnection, KafkaRecord};
use crate::RunParams;
use crate::burst;
use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Write a `Burst<KafkaRecord>` stream to Kafka.
///
/// Connects once at startup and produces every [`KafkaRecord`] in each burst
/// concurrently: all records in a burst are handed to the producer up front
/// and awaited together, so burst latency is one broker roundtrip rather than
/// N. Any delivery failure terminates the consumer and propagates to the graph.
/// The producer is flushed when the upstream ends to drain any queued retries.
#[must_use]
pub fn kafka_pub(
    connection: KafkaConnection,
    upstream: &Rc<dyn Stream<Burst<KafkaRecord>>>,
) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(
        move |_ctx: RunParams, mut source: Pin<Box<dyn FutStream<Burst<KafkaRecord>>>>| async move {
            let producer: FutureProducer = ClientConfig::new()
                .set("bootstrap.servers", &connection.brokers)
                .set("message.timeout.ms", "5000")
                .create()
                .map_err(|e| anyhow::anyhow!("kafka producer create failed: {e}"))?;

            while let Some((_time, burst)) = source.next().await {
                let mut inflight = FuturesUnordered::new();
                for record in &burst {
                    let mut fut_record = FutureRecord::to(&record.topic).payload(&record.value);
                    if let Some(ref key) = record.key {
                        fut_record = fut_record.key(key.as_slice());
                    }
                    inflight.push(Box::pin(producer.send(fut_record, SEND_TIMEOUT)));
                }
                while let Some(result) = inflight.next().await {
                    result.map_err(|(e, _)| anyhow::anyhow!("kafka produce failed: {e}"))?;
                }
            }

            producer
                .flush(FLUSH_TIMEOUT)
                .map_err(|e| anyhow::anyhow!("kafka flush failed: {e}"))?;

            Ok(())
        },
    ))
}

/// Extension trait providing a fluent API for writing streams to Kafka.
///
/// Implemented for both `Burst<KafkaRecord>` (multi-item) and `KafkaRecord` (single-item)
/// streams, so burst wrapping is never required in user code.
pub trait KafkaPubOperators {
    /// Write this stream to Kafka.
    #[must_use]
    fn kafka_pub(self: &Rc<Self>, conn: KafkaConnection) -> Rc<dyn Node>;
}

impl KafkaPubOperators for dyn Stream<Burst<KafkaRecord>> {
    fn kafka_pub(self: &Rc<Self>, conn: KafkaConnection) -> Rc<dyn Node> {
        kafka_pub(conn, self)
    }
}

impl KafkaPubOperators for dyn Stream<KafkaRecord> {
    fn kafka_pub(self: &Rc<Self>, conn: KafkaConnection) -> Rc<dyn Node> {
        let burst_stream = self.map(|record| burst![record]);
        kafka_pub(conn, &burst_stream)
    }
}
