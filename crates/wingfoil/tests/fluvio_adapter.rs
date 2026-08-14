//! fluvio adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/fluvio_integration.rs`
//! behind the `fluvio-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil --features fluvio
//! ```
#![cfg(feature = "fluvio")]

use std::time::Duration;

use wingfoil::adapters::fluvio::{
    FluvioConnection, FluvioRecord, FluvioSinkOps, FluvioSourceConfig, fluvio_source, fluvio_sub,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

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
/// check is pure, so wingfoil fails at wiring rather than deferring it into the
/// producer future the way legacy did.
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
/// the parity of legacy `test_connection_refused`.
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

// ---- fluvio_source: mode dispatch (no cluster needed) ----

fn realtime() -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    }
}

/// A `HistoricalFrom` run errors at wiring: fluvio has no bounded historical
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
    let cfg = FluvioSourceConfig::new().live(None);
    let err = fluvio_source(&g, params, "127.0.0.1:9003", "topic", 0, cfg)
        .err()
        .expect("HistoricalFrom must be rejected — fluvio has no historical half");
    let msg = format!("{err:#}");
    assert!(msg.contains("fluvio_source"), "names the adapter: {msg}");
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
    let err = fluvio_source(
        &g,
        realtime(),
        "127.0.0.1:9003",
        "topic",
        0,
        FluvioSourceConfig::new(),
    )
    .err()
    .expect("RealTime without a live config must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("fluvio_source"), "names the adapter: {msg}");
    assert!(msg.contains("live"), "names the missing live half: {msg}");
}

/// A `RealTime` run with a live half dispatches to the `fluvio_sub` mechanism,
/// which connects lazily in its producer task — so wiring succeeds (the graph is
/// never run here).
#[test]
fn source_dispatches_to_sub_under_realtime() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = FluvioSourceConfig::new().live(Some(7));
    let wired = fluvio_source(&g, realtime(), "127.0.0.1:9003", "topic", 0, cfg);
    assert!(
        wired.is_ok(),
        "RealTime with a live config wires (sub connects lazily): {:?}",
        wired.err()
    );
}

/// The realtime dispatch reaches `fluvio_sub` itself, not a copy of it: the
/// primitive's negative-`start_offset` validation fires through `fluvio_source`.
#[test]
fn source_inherits_sub_offset_validation() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = FluvioSourceConfig::new().live(Some(-1));
    let err = fluvio_source(&g, realtime(), "127.0.0.1:9003", "topic", 0, cfg)
        .err()
        .expect("a negative start_offset must be rejected by the sub mechanism");
    assert!(
        format!("{err:#}").contains("start_offset"),
        "dispatched to fluvio_sub's validation: {err:#}"
    );
}
