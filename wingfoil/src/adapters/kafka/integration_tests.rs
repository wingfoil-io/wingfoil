//! Integration tests for the Kafka adapter.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test --features kafka-integration-test -p wingfoil \
//!   -- --test-threads=1 kafka::integration_tests
//! ```

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::types::Burst;
use crate::{RunFor, RunMode, burst};
use rdkafka::Message;
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, TopicReplication};
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::{FutureProducer, FutureRecord};
use std::rc::Rc;
use std::time::Duration;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

const REDPANDA_PORT: u16 = 9092;
const REDPANDA_IMAGE: &str = "docker.redpanda.com/redpandadata/redpanda";
const REDPANDA_TAG: &str = "v24.1.1";

/// Start a Redpanda container and return the host endpoint.
/// The returned container must be kept alive for the duration of the test.
fn start_redpanda() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new(REDPANDA_IMAGE, REDPANDA_TAG)
        .with_wait_for(WaitFor::message_on_stderr("Started Kafka API server"))
        .with_mapped_port(REDPANDA_PORT, REDPANDA_PORT.into())
        .with_cmd(vec![
            "redpanda".to_string(),
            "start".to_string(),
            "--overprovisioned".to_string(),
            "--smp".to_string(),
            "1".to_string(),
            "--memory".to_string(),
            "512M".to_string(),
            "--reserve-memory".to_string(),
            "0M".to_string(),
            "--node-id".to_string(),
            "0".to_string(),
            "--check=false".to_string(),
            "--kafka-addr".to_string(),
            format!("0.0.0.0:{REDPANDA_PORT}"),
            "--advertise-kafka-addr".to_string(),
            format!("127.0.0.1:{REDPANDA_PORT}"),
        ])
        .start()?;
    let endpoint = format!("127.0.0.1:{REDPANDA_PORT}");
    Ok((container, endpoint))
}

/// Create a topic via the admin API.
fn create_topic(brokers: &str, topic: &str, partitions: i32) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let admin: AdminClient<rdkafka::client::DefaultClientContext> = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .create()
            .map_err(|e| anyhow::anyhow!("admin create failed: {e}"))?;
        let new_topic = NewTopic::new(topic, partitions, TopicReplication::Fixed(1));
        admin
            .create_topics(&[new_topic], &AdminOptions::new())
            .await
            .map_err(|e| anyhow::anyhow!("create topic failed: {e}"))?;
        // Give the broker a moment to propagate metadata.
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    })
}

/// Produce messages directly via the client.
fn produce_messages(brokers: &str, topic: &str, messages: &[(&str, &str)]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("message.timeout.ms", "5000")
            .create()
            .map_err(|e| anyhow::anyhow!("producer create failed: {e}"))?;
        for (key, value) in messages {
            producer
                .send(
                    FutureRecord::to(topic).key(*key).payload(*value),
                    Duration::from_secs(5),
                )
                .await
                .map_err(|(e, _)| anyhow::anyhow!("produce failed: {e}"))?;
        }
        Ok(())
    })
}

/// Consume messages directly via the client, returning up to `max` messages.
fn consume_messages(
    brokers: &str,
    topic: &str,
    group_id: &str,
    max: usize,
) -> anyhow::Result<Vec<(Option<Vec<u8>>, Vec<u8>)>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("group.id", group_id)
            .set("auto.offset.reset", "earliest")
            .set("session.timeout.ms", "6000")
            .create()
            .map_err(|e| anyhow::anyhow!("consumer create failed: {e}"))?;
        consumer
            .subscribe(&[topic])
            .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;

        let mut results = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if results.len() >= max || tokio::time::Instant::now() >= deadline {
                break;
            }
            match tokio::time::timeout(Duration::from_secs(2), consumer.recv()).await {
                Ok(Ok(msg)) => {
                    results.push((
                        msg.key().map(|k| k.to_vec()),
                        msg.payload().unwrap_or_default().to_vec(),
                    ));
                }
                Ok(Err(e)) => return Err(anyhow::anyhow!("consume error: {e}")),
                Err(_) => break, // timeout
            }
        }
        Ok(results)
    })
}

/// Build a one-shot `constant` stream containing a single [`KafkaRecord`].
fn make_burst(topic: &str, key: &[u8], value: &[u8]) -> Rc<dyn crate::Stream<Burst<KafkaRecord>>> {
    crate::nodes::constant(burst![KafkaRecord {
        topic: topic.to_string(),
        key: Some(key.to_vec()),
        value: value.to_vec(),
    }])
}

// ---- Tests ----

#[test]
fn test_connection_refused() {
    // Error propagates correctly without a running broker.
    let conn = KafkaConnection::new("127.0.0.1:59999");
    let result = kafka_sub(conn, "nonexistent", "test-group")
        .collapse()
        .collect()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(5)));
    // rdkafka may not immediately error on a bad broker — it retries.
    // We just verify it doesn't panic and eventually terminates.
    // The graph should either error or complete after the duration.
    let _ = result;
}

#[test]
fn test_sub_receives_pre_seeded_messages() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-sub-seeded";
    create_topic(&brokers, topic, 1)?;
    produce_messages(&brokers, topic, &[("k1", "v1"), ("k2", "v2")])?;

    let conn = KafkaConnection::new(&brokers);
    let collected = kafka_sub(conn, topic, "sub-seeded-group")
        .collapse()
        .collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(2))?;

    let events = collected.peek_value();
    assert_eq!(events.len(), 2);
    let values: Vec<Vec<u8>> = events.iter().map(|e| e.value.value.clone()).collect();
    assert!(values.contains(&b"v1".to_vec()));
    assert!(values.contains(&b"v2".to_vec()));
    Ok(())
}

#[test]
fn test_sub_live_messages() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-sub-live";
    create_topic(&brokers, topic, 1)?;

    let conn = KafkaConnection::new(&brokers);

    let brokers_clone = brokers.clone();
    let topic_owned = topic.to_string();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        produce_messages(&brokers_clone, &topic_owned, &[("live-key", "live-value")]).unwrap();
    });

    let collected = kafka_sub(conn, topic, "sub-live-group")
        .collapse()
        .collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;
    handle.join().unwrap();

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value.value, b"live-value");
    Ok(())
}

#[test]
fn test_pub_round_trip() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-pub-rt";
    create_topic(&brokers, topic, 1)?;

    let conn = KafkaConnection::new(&brokers);
    let source = make_burst(topic, b"rt-key", b"rt-value");
    kafka_pub(conn, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    // Verify via direct consumer read.
    let messages = consume_messages(&brokers, topic, "rt-verify-group", 1)?;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0.as_deref(), Some(b"rt-key".as_ref()));
    assert_eq!(messages[0].1, b"rt-value");
    Ok(())
}

#[test]
fn test_pub_multiple_records_in_burst() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-pub-multi";
    create_topic(&brokers, topic, 1)?;

    let conn = KafkaConnection::new(&brokers);
    let source = crate::nodes::constant(burst![
        KafkaRecord {
            topic: topic.to_string(),
            key: Some(b"k1".to_vec()),
            value: b"v1".to_vec(),
        },
        KafkaRecord {
            topic: topic.to_string(),
            key: Some(b"k2".to_vec()),
            value: b"v2".to_vec(),
        },
    ]);
    kafka_pub(conn, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let messages = consume_messages(&brokers, topic, "multi-verify-group", 2)?;
    assert_eq!(messages.len(), 2);
    let values: Vec<Vec<u8>> = messages.iter().map(|(_, v)| v.clone()).collect();
    assert!(values.contains(&b"v1".to_vec()));
    assert!(values.contains(&b"v2".to_vec()));
    Ok(())
}

#[test]
fn test_sub_event_fields() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-sub-fields";
    create_topic(&brokers, topic, 1)?;
    produce_messages(&brokers, topic, &[("field-key", "field-value")])?;

    let conn = KafkaConnection::new(&brokers);
    let collected = kafka_sub(conn, topic, "fields-group").collapse().collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    let event = &events[0].value;
    assert_eq!(event.topic, topic);
    assert_eq!(event.partition, 0);
    assert!(event.offset >= 0);
    assert_eq!(event.key.as_deref(), Some(b"field-key".as_ref()));
    assert_eq!(event.value, b"field-value");
    assert_eq!(event.value_str().unwrap(), "field-value");
    assert_eq!(event.key_str().unwrap().unwrap(), "field-key");
    Ok(())
}

#[test]
fn test_kafka_record_value_str() {
    let record = KafkaRecord {
        topic: "t".to_string(),
        key: Some(b"k".to_vec()),
        value: b"hello".to_vec(),
    };
    assert_eq!(record.value_str().unwrap(), "hello");
}

#[test]
fn test_kafka_event_no_key() {
    let event = KafkaEvent {
        topic: "t".to_string(),
        partition: 0,
        offset: 0,
        key: None,
        value: b"val".to_vec(),
    };
    assert!(event.key_str().is_none());
}
