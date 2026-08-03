//! kafka adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/kafka_integration.rs` behind
//! the `kafka-integration-test` feature. Run these with:
//! ```sh
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features kafka
//! ```
#![cfg(feature = "kafka")]

use std::time::Duration;

use wingfoil::adapters::kafka::{
    KafkaConnection, KafkaRecord, KafkaSinkOps, KafkaSourceConfig, kafka_source, kafka_sub,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// `kafka_sub` rejects a `HistoricalFrom` run at wiring time — the live,
/// unbounded, wall-clock consumer has no historical timeline to replay, and the
/// historical channel path would block-collect its never-ending `recv()` loop up
/// front and deadlock at `start`. The error must name the adapter rather than
/// hang.
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::ZERO),
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::ZERO,
    };
    let err = match kafka_sub(
        &g,
        params.run_mode,
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
/// the parity of legacy `test_connection_refused`.
#[test]
fn sub_connection_refused_terminates() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(Duration::from_secs(3)),
        start_time: NanoTime::ZERO,
    };
    let _events = kafka_sub(
        &g,
        params.run_mode,
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
/// — the parity of legacy's producer-create path. The record's target topic is
/// carried on each [`KafkaRecord`], so the sink can be built from a plain
/// `Stream<KafkaRecord>` too.
#[test]
fn pub_wires_from_single_record_stream() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .constant(KafkaRecord {
            topic: "t".to_string(),
            key: Some(b"k".to_vec()),
            value: b"v".to_vec(),
        })
        .kafka_pub("127.0.0.1:9092")
        .expect("kafka_pub wires from a single-record stream");
}

// ---- kafka_source: mode dispatch (no broker needed) ----

fn realtime_params() -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    }
}

/// A `HistoricalFrom` run errors at wiring: kafka has no bounded historical
/// reader, so there is no half to dispatch to. The message must name the
/// adapter and say the historical half is unimplemented — *not* point at a
/// `.historical(..)` builder that does not exist.
#[test]
fn source_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::ZERO),
        run_for: RunFor::Cycles(10),
        start_time: NanoTime::ZERO,
    };
    let cfg = KafkaSourceConfig::new().live("group");
    let err = kafka_source(
        &g,
        params,
        KafkaConnection::new("127.0.0.1:9092"),
        "topic",
        cfg,
    )
    .err()
    .expect("HistoricalFrom must be rejected — kafka has no historical half");
    let msg = format!("{err:#}");
    assert!(msg.contains("kafka_source"), "names the adapter: {msg}");
    assert!(
        msg.contains("historical half"),
        "names the unimplemented historical half: {msg}"
    );
    assert!(
        !msg.contains(".historical("),
        "must not point at a builder that does not exist: {msg}"
    );
}

/// A `RealTime` run with no live half errors at wiring, naming the missing half.
#[test]
fn source_missing_live_half_errors_under_realtime() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let err = kafka_source(
        &g,
        realtime_params(),
        KafkaConnection::new("127.0.0.1:9092"),
        "topic",
        KafkaSourceConfig::new(),
    )
    .err()
    .expect("RealTime without a live config must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("kafka_source"), "names the adapter: {msg}");
    assert!(msg.contains("live"), "names the missing live half: {msg}");
}

/// A `RealTime` run with a live half dispatches to the `kafka_sub` mechanism,
/// whose consumer is created in its producer task at run start — so wiring
/// succeeds (the graph is never run here).
#[test]
fn source_dispatches_to_sub_under_realtime() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = KafkaSourceConfig::new().live("test-group");
    let wired = kafka_source(
        &g,
        realtime_params(),
        KafkaConnection::new("127.0.0.1:9092"),
        "topic",
        cfg,
    );
    assert!(
        wired.is_ok(),
        "RealTime with a live config wires (consumer is created at run start): {:?}",
        wired.err()
    );
}
