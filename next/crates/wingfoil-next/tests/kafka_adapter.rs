//! kafka adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/kafka_integration.rs` behind
//! the `kafka-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features kafka
//! ```
#![cfg(feature = "kafka")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::kafka::{KafkaConnection, KafkaRecord, KafkaSinkOps, kafka_sub};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

/// `kafka_sub` rejects a `HistoricalFrom` run at wiring time — the live,
/// unbounded, wall-clock consumer has no historical timeline to replay, and the
/// historical channel path would block-collect its never-ending `recv()` loop up
/// front and deadlock at `start`. The error must name the adapter rather than
/// hang.
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::ZERO),
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::ZERO,
    };
    let err = match kafka_sub(
        &g,
        rt.handle(),
        params,
        KafkaConnection::new("127.0.0.1:9092"),
        "topic",
        "group",
    ) {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("kafka_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// An unreachable broker must not hang or panic the consumer's run — with no
/// broker up, librdkafka retries in the background, so a bounded-duration run
/// simply terminates (with events or an error) rather than deadlocking. This is
/// the parity of classic `test_connection_refused`.
#[test]
fn sub_connection_refused_terminates() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(Duration::from_secs(3)),
        start_time: NanoTime::ZERO,
    };
    let _events = kafka_sub(
        &g,
        rt.handle(),
        params,
        KafkaConnection::new("127.0.0.1:59999"),
        "nonexistent",
        "test-group",
    )
    .expect("realtime kafka_sub wires without error")
    .collapse_accumulate();

    // rdkafka retries a bad broker rather than erroring immediately; we only
    // verify the run terminates within the duration without panicking.
    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)));
}

/// The sink wires without error against a plausible broker string (librdkafka
/// connects lazily, so producer creation succeeds even when the broker is down)
/// — the parity of classic's producer-create path. The record's target topic is
/// carried on each [`KafkaRecord`], so the sink can be built from a plain
/// `Stream<KafkaRecord>` too.
#[test]
fn pub_wires_from_single_record_stream() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let _sink = g
        .constant(KafkaRecord {
            topic: "t".to_string(),
            key: Some(b"k".to_vec()),
            value: b"v".to_vec(),
        })
        .kafka_pub(rt.handle(), "127.0.0.1:9092")
        .expect("kafka_pub wires from a single-record stream");
}
