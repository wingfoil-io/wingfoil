//! Kafka producer consumer — writes messages to Kafka topics.

use super::{KafkaConnection, KafkaRecord};
use crate::RunParams;
use crate::burst;
use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;
use futures::StreamExt;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// Write a `Burst<KafkaRecord>` stream to Kafka.
///
/// Connects once at startup and produces one message per [`KafkaRecord`] in each burst.
/// Each record specifies its target topic, allowing writes to multiple topics from
/// a single consumer. Messages are produced with delivery confirmation; any
/// delivery failure terminates the consumer and propagates to the graph.
#[must_use]
pub fn kafka_pub(
    connection: KafkaConnection,
    upstream: &Rc<dyn Stream<Burst<KafkaRecord>>>,
) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(
        move |_ctx: RunParams, source: Pin<Box<dyn FutStream<Burst<KafkaRecord>>>>| async move {
            let producer: FutureProducer = ClientConfig::new()
                .set("bootstrap.servers", &connection.brokers)
                .set("message.timeout.ms", "5000")
                .create()
                .map_err(|e| anyhow::anyhow!("kafka producer create failed: {e}"))?;

            let mut source = source;
            while let Some((_time, burst)) = source.next().await {
                for record in burst {
                    let mut fut_record = FutureRecord::to(&record.topic).payload(&record.value);
                    if let Some(ref key) = record.key {
                        fut_record = fut_record.key(key.as_slice());
                    }
                    producer
                        .send(fut_record, Duration::from_secs(5))
                        .await
                        .map_err(|(e, _)| anyhow::anyhow!("kafka produce failed: {e}"))?;
                }
            }

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
