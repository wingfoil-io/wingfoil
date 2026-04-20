//! Kafka consumer producer — streams messages from a Kafka topic.

use super::{KafkaConnection, KafkaEvent};
use crate::nodes::{RunParams, produce_async};
use crate::types::*;
use rdkafka::Message;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::error::KafkaError;
use rdkafka::types::RDKafkaErrorCode;
use std::rc::Rc;

/// Consume messages from a Kafka `topic` as [`KafkaEvent`]s.
///
/// Creates a consumer in the given `group_id` consumer group and subscribes to
/// the topic. Messages are emitted as they arrive from the broker.
///
/// Emits `Burst<KafkaEvent>`. Use `.collapse()` for single-event processing.
///
/// # Consumer group
///
/// The `group_id` determines offset tracking. Using the same group across
/// multiple graph runs continues from the last committed offset. Use a unique
/// group to read from the beginning each time.
///
/// # Delivery semantics
///
/// This consumer uses `enable.auto.commit = true`, which provides **at-most-once**
/// delivery: offsets are committed periodically in the background, so if the graph
/// errors after a message is fetched but before it is fully processed, that message
/// will not be re-delivered. If you need at-least-once guarantees, disable auto-commit
/// and manage offsets manually via the rdkafka API.
#[must_use]
pub fn kafka_sub(
    connection: KafkaConnection,
    topic: impl Into<String>,
    group_id: impl Into<String>,
) -> Rc<dyn Stream<Burst<KafkaEvent>>> {
    let topic = topic.into();
    let group_id = group_id.into();
    produce_async(move |_ctx: RunParams| async move {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &connection.brokers)
            .set("group.id", &group_id)
            .set("auto.offset.reset", "earliest")
            .set("enable.auto.commit", "true")
            .set("session.timeout.ms", "6000")
            .create()
            .map_err(|e| anyhow::anyhow!("kafka consumer create failed: {e}"))?;

        consumer
            .subscribe(&[&topic])
            .map_err(|e| anyhow::anyhow!("kafka subscribe failed: {e}"))?;

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
                        yield Err(anyhow::anyhow!("kafka consume error: {e}"));
                        break;
                    }
                }
            }
        })
    })
}

/// `UnknownTopicOrPartition` is reported while the broker is still catching up on
/// topic metadata (or the topic is pending auto-create). librdkafka keeps
/// retrying the subscription in the background, so we swallow it and wait for
/// the next event rather than terminating the stream.
fn is_transient_subscribe_error(err: &KafkaError) -> bool {
    matches!(
        err,
        KafkaError::MessageConsumption(RDKafkaErrorCode::UnknownTopicOrPartition)
    )
}
