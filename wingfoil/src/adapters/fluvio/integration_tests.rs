//! Integration tests for the Fluvio adapter.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test --features fluvio-integration-test -p wingfoil \
//!   -- --test-threads=1 fluvio::integration_tests
//! ```
//!
//! # Container startup
//!
//! Tests spin up `infinyon/fluvio:0.18.1` via testcontainers using host networking.
//! A shell script starts the SC then the SPU so topic creation and produce/consume
//! work end-to-end. Host networking is required so the SPU's public endpoint
//! (registered with the SC as `127.0.0.1:9010`) is reachable by the test process.

use super::*;
use crate::nodes::{NodeOperators, StreamOperators, constant};
use crate::{RunFor, RunMode, burst};
use fluvio::consumer::ConsumerConfigExt;
use fluvio::metadata::topic::TopicSpec;
use fluvio::{Fluvio, FluvioAdmin, FluvioClusterConfig, Offset};
use futures::StreamExt;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

const FLUVIO_SC_PORT: u16 = 9003;
const FLUVIO_SPU_PORT: u16 = 9010;
const FLUVIO_IMAGE: &str = "infinyon/fluvio";
const FLUVIO_TAG: &str = "0.18.1";

/// Start a full Fluvio local cluster (SC + SPU) using host networking.
///
/// Host networking is required so the SPU's public address (`127.0.0.1:9010`),
/// which the SC hands to clients, is reachable from the test process outside
/// the container.  The SC starts first; the SPU waits 5 s then registers with it.
fn start_fluvio() -> anyhow::Result<(impl Drop, String)> {
    let startup_cmd = format!(
        "/fluvio-run sc --local /tmp/fluvio & \
         sleep 5 && \
         /fluvio-run spu \
           --id 5001 \
           --public-server 0.0.0.0:{FLUVIO_SPU_PORT} \
           --private-server 0.0.0.0:{FLUVIO_SPU_PORT} \
           --sc-addr 127.0.0.1:{FLUVIO_SC_PORT} \
           --log-base-dir /tmp/fluvio"
    );

    let container = GenericImage::new(FLUVIO_IMAGE, FLUVIO_TAG)
        // Wait long enough for SC + SPU to start and the SPU to register.
        .with_wait_for(WaitFor::millis(15_000))
        .with_cmd(vec!["/bin/sh", "-c", &startup_cmd])
        .with_host_config_modifier(|hc| {
            hc.network_mode = Some("host".to_string());
        })
        .start()?;

    // With host networking the SC is reachable directly on the host port.
    let endpoint = format!("127.0.0.1:{FLUVIO_SC_PORT}");
    Ok((container, endpoint))
}

/// Create a topic using FluvioAdmin with a throwaway async runtime.
fn create_topic(endpoint: &str, topic: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let admin = FluvioAdmin::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio admin connect failed: {e}"))?;
        admin
            .create::<TopicSpec>(
                topic.to_string(),
                false,
                TopicSpec::new_computed(1, 1, None),
            )
            .await
            .map_err(|e| anyhow::anyhow!("topic create failed: {e}"))?;
        Ok(())
    })
}

/// Seed records into a topic directly via the Fluvio producer.
fn seed_records(endpoint: &str, topic: &str, records: &[(&str, &[u8])]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let client = Fluvio::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio connect failed: {e}"))?;
        let producer = client
            .topic_producer(topic)
            .await
            .map_err(|e| anyhow::anyhow!("producer create failed: {e}"))?;
        for (key, value) in records {
            producer
                .send(*key, *value)
                .await
                .map_err(|e| anyhow::anyhow!("send failed: {e}"))?;
        }
        producer
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("flush failed: {e}"))?;
        Ok(())
    })
}

/// Read records from a topic directly via the Fluvio consumer.
/// Returns all records currently available (up to `limit`).
fn read_records(
    endpoint: &str,
    topic: &str,
    limit: usize,
) -> anyhow::Result<Vec<(Option<Vec<u8>>, Vec<u8>)>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let client = Fluvio::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio connect failed: {e}"))?;
        let consumer_config = ConsumerConfigExt::builder()
            .topic(topic)
            .partition(0u32)
            .offset_start(Offset::beginning())
            .build()
            .map_err(|e| anyhow::anyhow!("consumer config failed: {e}"))?;
        let mut stream = client
            .consumer_with_config(consumer_config)
            .await
            .map_err(|e| anyhow::anyhow!("consumer create failed: {e}"))?;
        let mut results = Vec::new();
        while results.len() < limit {
            match stream.next().await {
                Some(Ok(record)) => {
                    results.push((record.key().map(|k| k.to_vec()), record.value().to_vec()));
                }
                Some(Err(e)) => {
                    return Err(anyhow::anyhow!("record error: {e:?}"));
                }
                None => break,
            }
        }
        Ok(results)
    })
}

// ---- Tests ----

#[test]
fn test_connection_refused() {
    // Error propagates correctly when Fluvio SC is not reachable.
    let conn = FluvioConnection::new("127.0.0.1:59999");
    let result = fluvio_sub(conn, "any-topic", 0, None)
        .collapse()
        .collect()
        .run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(result.is_err(), "expected connection error");
}

#[test]
fn test_sub_from_beginning() -> anyhow::Result<()> {
    // Records pre-seeded before the consumer starts are received from offset 0.
    let (_container, endpoint) = start_fluvio()?;
    let topic = "sub-from-beginning";
    create_topic(&endpoint, topic)?;
    seed_records(&endpoint, topic, &[("k1", b"hello"), ("k2", b"world")])?;

    let conn = FluvioConnection::new(&endpoint);
    let collected = fluvio_sub(conn, topic, 0, None).collect();
    // RunFor::Cycles(2): one burst per record (two records seeded)
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(2))?;

    let bursts = collected.peek_value();
    let events: Vec<FluvioEvent> = bursts
        .into_iter()
        .flat_map(|b| b.value.into_iter())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value, b"hello");
    assert_eq!(events[1].value, b"world");
    Ok(())
}

#[test]
fn test_pub_round_trip() -> anyhow::Result<()> {
    // fluvio_pub writes records; verify via direct consumer read.
    let (_container, endpoint) = start_fluvio()?;
    let topic = "pub-round-trip";
    create_topic(&endpoint, topic)?;

    let conn = FluvioConnection::new(&endpoint);
    let source = constant(burst![
        FluvioRecord::with_key("greeting", b"hello".to_vec()),
        FluvioRecord::new(b"world".to_vec()),
    ]);
    fluvio_pub(conn, topic, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let records = read_records(&endpoint, topic, 2)?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].1, b"hello");
    assert_eq!(records[1].1, b"world");
    Ok(())
}

#[test]
fn test_sub_live_stream() -> anyhow::Result<()> {
    // Records produced after the consumer starts are received.
    let (_container, endpoint) = start_fluvio()?;
    let topic = "sub-live-stream";
    create_topic(&endpoint, topic)?;

    let endpoint_clone = endpoint.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        seed_records(&endpoint_clone, topic, &[("live", b"event")]).unwrap();
    });

    let conn = FluvioConnection::new(&endpoint);
    let collected = fluvio_sub(conn, topic, 0, None).collapse().collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;
    handle.join().unwrap();

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value.value, b"event");
    Ok(())
}

#[test]
fn test_pub_keyless_record() -> anyhow::Result<()> {
    // Keyless records (RecordKey::NULL) are written and read back correctly.
    let (_container, endpoint) = start_fluvio()?;
    let topic = "keyless-records";
    create_topic(&endpoint, topic)?;

    let conn = FluvioConnection::new(&endpoint);
    let source = constant(burst![FluvioRecord::new(b"no-key".to_vec())]);
    fluvio_pub(conn, topic, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let records = read_records(&endpoint, topic, 1)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, None, "key should be None for keyless record");
    assert_eq!(records[0].1, b"no-key");
    Ok(())
}

#[test]
fn test_pub_keyed_record() -> anyhow::Result<()> {
    // String keys are stored and readable from the consumer.
    let (_container, endpoint) = start_fluvio()?;
    let topic = "keyed-records";
    create_topic(&endpoint, topic)?;

    let conn = FluvioConnection::new(&endpoint);
    let source = constant(burst![FluvioRecord::with_key(
        "my-key",
        b"my-value".to_vec()
    )]);
    fluvio_pub(conn, topic, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let records = read_records(&endpoint, topic, 1)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0.as_deref(), Some(b"my-key".as_ref()));
    assert_eq!(records[0].1, b"my-value");
    Ok(())
}

#[test]
fn test_sub_from_absolute_offset() -> anyhow::Result<()> {
    // Consumer with a non-zero start_offset skips earlier records.
    let (_container, endpoint) = start_fluvio()?;
    let topic = "sub-abs-offset";
    create_topic(&endpoint, topic)?;
    seed_records(
        &endpoint,
        topic,
        &[("k0", b"first"), ("k1", b"second"), ("k2", b"third")],
    )?;

    // Start from offset 1 — should receive "second" and "third" only.
    let conn = FluvioConnection::new(&endpoint);
    let collected = fluvio_sub(conn, topic, 0, Some(1)).collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(2))?;

    let events: Vec<FluvioEvent> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value.into_iter())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value, b"second");
    assert_eq!(events[1].value, b"third");
    Ok(())
}
