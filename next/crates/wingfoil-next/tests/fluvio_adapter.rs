//! fluvio adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/fluvio_integration.rs`
//! behind the `fluvio-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features fluvio
//! ```
#![cfg(feature = "fluvio")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::fluvio::{FluvioConnection, FluvioRecord, FluvioSinkOps, fluvio_sub};
use wingfoil_next::prelude::*;

/// `fluvio_sub` rejects a `HistoricalFrom` run at wiring time — the live,
/// unbounded, wall-clock consumer has no historical timeline to replay, and the
/// historical channel path would block-collect its never-ending record stream up
/// front and deadlock at `start`. The error must name the adapter rather than
/// hang.
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let err = match fluvio_sub(
        &g,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        "127.0.0.1:9003",
        "topic",
        0,
        None,
    ) {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("fluvio_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// A negative `start_offset` is invalid (`Offset::absolute` rejects it). The
/// check is pure, so next fails at wiring rather than deferring it into the
/// producer future the way classic did.
#[test]
fn sub_rejects_negative_offset() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let err = match fluvio_sub(
        &g,
        RunMode::RealTime,
        FluvioConnection::new("127.0.0.1:9003"),
        "topic",
        0,
        Some(-1),
    ) {
        Ok(_) => panic!("a negative start_offset must be rejected"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("fluvio_sub"), "names the adapter: {msg}");
    assert!(msg.contains("non-negative"), "explains the bound: {msg}");
}

/// An unreachable SC must abort the run with context rather than hang or panic —
/// the parity of classic `test_connection_refused`.
#[test]
fn sub_connection_refused_aborts_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _events = fluvio_sub(
        &g,
        RunMode::RealTime,
        "127.0.0.1:59999",
        "any-topic",
        0,
        None,
    )
    .expect("realtime fluvio_sub wires without error")
    .collapse_accumulate();

    let mut runner = g.build();
    let err = runner
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(10)))
        .expect_err("an unreachable SC must abort the run");
    let msg = format!("{err:#}");
    assert!(msg.contains("fluvio_sub"), "names the adapter: {msg}");
}

/// The sink wires without error against an unreachable cluster: the connection
/// and producer are established lazily on the first burst, so wiring does no
/// I/O. The single-record convenience impl is exercised here too.
#[test]
fn pub_wires_from_single_record_stream() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .constant(FluvioRecord::with_key("k", b"v".to_vec()))
        .fluvio_pub("127.0.0.1:59999", "any-topic", None)
        .expect("fluvio_pub wires from a single-record stream");
}

/// The burst sink form wires the same way, and a write against an unreachable
/// cluster aborts the run with context (the lazy connect happens in the consumer
/// task, so the failure surfaces during the run, not at wiring).
#[test]
fn pub_connection_refused_aborts_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .constant(burst![
            FluvioRecord::with_key("greeting", b"hello".to_vec()),
            FluvioRecord::new(b"world".to_vec()),
        ])
        .fluvio_pub("127.0.0.1:59999", "any-topic", None)
        .expect("fluvio_pub wires against an unreachable cluster");

    let mut runner = g.build();
    let err = runner
        .run(RunMode::RealTime, RunFor::Cycles(2))
        .expect_err("an unreachable SC must abort the run");
    let msg = format!("{err:#}");
    assert!(msg.contains("fluvio_pub"), "names the adapter: {msg}");
}
