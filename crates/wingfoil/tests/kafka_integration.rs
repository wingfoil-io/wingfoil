//! Integration tests for the kafka adapter — parity port of the legacy
//! `legacy/wingfoil/src/adapters/kafka/integration_tests.rs`.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test -p wingfoil --features kafka-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "kafka-integration-test")]

use std::time::Duration;

use rdkafka::Message;
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, TopicReplication};
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::{FutureProducer, FutureRecord};
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
use wingfoil::adapters::kafka::{
    KafkaConnection, KafkaEvent, KafkaRecord, KafkaSinkOps, kafka_sub,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const REDPANDA_IMAGE: &str = "docker.redpanda.com/redpandadata/redpanda";
const REDPANDA_TAG: &str = "v24.1.1";

/// Pick an OS-assigned free TCP port.
///
/// The port is released when the returned listener drops, so there is a small
/// TOCTOU window before the container binds it. Good enough for tests.
fn free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Start a Redpanda container and return the host endpoint.
///
/// Uses a fresh OS-assigned port for each container. Redpanda's
/// `--advertise-kafka-addr` must match the address clients connect to, so the
/// container binds the same port internally, mapped 1:1 to the host. This avoids
/// collisions with any broker a developer might already be running on 9092.
/// Parallel tests on the same machine are serialized by `--test-threads=1`.
///
/// The returned container must be kept alive for the duration of the test.
fn start_redpanda() -> anyhow::Result<(impl Drop, String)> {
    let port = free_port()?;
    let container = GenericImage::new(REDPANDA_IMAGE, REDPANDA_TAG)
        .with_wait_for(WaitFor::message_on_stderr("Started Kafka API server"))
        .with_mapped_port(port, port.into())
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
            format!("0.0.0.0:{port}"),
            "--advertise-kafka-addr".to_string(),
            format!("127.0.0.1:{port}"),
        ])
        .start()?;
    let endpoint = format!("127.0.0.1:{port}");
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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
                // A `recv()` timeout is NOT end-of-topic — it is the normal
                // shape of a fresh consumer group's first poll, which has to
                // find the group coordinator, join, and take its partition
                // assignment before any fetch can return. On a loaded CI runner
                // that routinely exceeds one 2 s slice. Keep polling until the
                // overall deadline instead of giving up on the first slow slice;
                // returning early here reports an empty topic when the records
                // are in fact present, which is what made
                // `test_pub_multiple_records_in_burst` flaky.
                Err(_) => continue,
            }
        }
        Ok(results)
    })
}

/// Consume a topic with `kafka_sub` for `duration`, returning every event seen.
///
/// Collects bursts losslessly via `collapse_accumulate` (never dropping events
/// that arrive within one graph cycle — exactly what happens the first time
/// after a consumer-group rebalance), then reads the accumulated `Vec` off the
/// runner. Uses `RunFor::Duration` so rebalance + delivery have time to complete.
fn consume_with_sub(
    brokers: &str,
    topic: &str,
    group: &str,
    secs: u64,
) -> anyhow::Result<Vec<KafkaEvent>> {
    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(Duration::from_secs(secs)),
        start_time: NanoTime::ZERO,
    };
    let events = kafka_sub(
        &g,
        params.run_mode,
        KafkaConnection::new(brokers),
        topic,
        group,
    )?
    .collapse_accumulate();
    let mut runner = g.build();
    runner.run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(secs)),
    )?;
    Ok(runner.value(&events))
}

// ---- Source tests ----

#[test]
fn test_sub_receives_pre_seeded_messages() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-sub-seeded";
    create_topic(&brokers, topic, 1)?;
    produce_messages(&brokers, topic, &[("k1", "v1"), ("k2", "v2")])?;

    let events = consume_with_sub(&brokers, topic, "sub-seeded-group", 20)?;
    assert!(
        events.len() >= 2,
        "expected at least 2 events, got {}",
        events.len()
    );
    let values: Vec<Vec<u8>> = events.iter().map(|e| e.value.clone()).collect();
    assert!(values.contains(&b"v1".to_vec()));
    assert!(values.contains(&b"v2".to_vec()));
    Ok(())
}

#[test]
fn test_sub_live_messages() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-sub-live";
    create_topic(&brokers, topic, 1)?;

    let brokers_clone = brokers.clone();
    let topic_owned = topic.to_string();
    let handle = std::thread::spawn(move || {
        // Give the consumer a few seconds to subscribe and rebalance before
        // producing, so we exercise the live-stream path.
        std::thread::sleep(Duration::from_secs(3));
        produce_messages(&brokers_clone, &topic_owned, &[("live-key", "live-value")]).unwrap();
    });

    let events = consume_with_sub(&brokers, topic, "sub-live-group", 20)?;
    handle.join().unwrap();

    assert!(!events.is_empty(), "expected at least 1 live event, got 0");
    assert_eq!(events[0].value, b"live-value");
    Ok(())
}

#[test]
fn test_sub_event_fields() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-sub-fields";
    create_topic(&brokers, topic, 1)?;
    produce_messages(&brokers, topic, &[("field-key", "field-value")])?;

    let events = consume_with_sub(&brokers, topic, "fields-group", 20)?;
    assert!(!events.is_empty(), "expected at least 1 event, got 0");
    let event = &events[0];
    assert_eq!(event.topic, topic);
    assert_eq!(event.partition, 0);
    assert!(event.offset >= 0);
    assert_eq!(event.key.as_deref(), Some(b"field-key".as_ref()));
    assert_eq!(event.value, b"field-value");
    assert_eq!(event.value_str().unwrap(), "field-value");
    assert_eq!(event.key_str().unwrap().unwrap(), "field-key");
    Ok(())
}

// ---- Sink tests ----

#[test]
fn test_pub_round_trip() -> anyhow::Result<()> {
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-pub-rt";
    create_topic(&brokers, topic, 1)?;

    {
        let rt = tokio::runtime::Runtime::new()?;
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g
            .constant(burst![KafkaRecord {
                topic: topic.to_string(),
                key: Some(b"rt-key".to_vec()),
                value: b"rt-value".to_vec(),
            }])
            .kafka_pub(KafkaConnection::new(&brokers))?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

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

    {
        let rt = tokio::runtime::Runtime::new()?;
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g
            .constant(burst![
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
            ])
            .kafka_pub(KafkaConnection::new(&brokers))?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

    let messages = consume_messages(&brokers, topic, "multi-verify-group", 2)?;
    let values: Vec<Vec<u8>> = messages.iter().map(|(_, v)| v.clone()).collect();
    // Report what actually arrived: 0 of 2 points at the consumer side (no
    // assignment within the deadline), whereas 1 of 2 would point at the
    // producer dropping part of the burst — very different bugs.
    assert_eq!(
        messages.len(),
        2,
        "expected both burst records, got {}: {:?}",
        messages.len(),
        values
    );
    assert!(values.contains(&b"v1".to_vec()));
    assert!(values.contains(&b"v2".to_vec()));
    Ok(())
}

#[test]
fn test_pub_round_trip_via_sub() -> anyhow::Result<()> {
    // Produce with kafka_pub, then read back the same records with kafka_sub —
    // both adapter directions in one flow.
    let (_container, brokers) = start_redpanda()?;
    let topic = "test-pub-sub-rt";
    create_topic(&brokers, topic, 1)?;

    {
        let rt = tokio::runtime::Runtime::new()?;
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g
            .constant(burst![KafkaRecord {
                topic: topic.to_string(),
                key: Some(b"key".to_vec()),
                value: b"payload".to_vec(),
            }])
            .kafka_pub(KafkaConnection::new(&brokers))?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

    let events = consume_with_sub(&brokers, topic, "pub-sub-rt-group", 20)?;
    assert!(
        !events.is_empty(),
        "kafka_sub should read the produced record"
    );
    assert!(events.iter().any(|e| e.value == b"payload"));
    Ok(())
}
