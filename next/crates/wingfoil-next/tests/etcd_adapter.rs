//! etcd adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/etcd_integration.rs` behind
//! the `etcd-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features etcd
//! ```
#![cfg(feature = "etcd")]

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::etcd::{EtcdConnection, EtcdEntry, EtcdSinkOps, etcd_sub};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

/// An unreachable endpoint must abort the source's run rather than hang or panic
/// — the parity of classic `test_connection_refused`.
#[test]
fn sub_connection_refused_aborts_the_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::ZERO,
    };
    let conn = EtcdConnection::new("http://127.0.0.1:59999");
    let _events = etcd_sub(&g, params.run_mode, conn, "/x/")
        .expect("realtime etcd_sub wires without error")
        .collapse_accumulate();

    let mut runner = g.build();
    let result = runner.run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        result.is_err(),
        "an unreachable etcd endpoint must abort the run"
    );
}

/// `etcd_sub` rejects a `HistoricalFrom` run at wiring time — the live,
/// unbounded, wall-clock watch has no historical timeline to replay, and the
/// historical channel path would deadlock at `start`. The error must name the
/// adapter rather than hang.
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::ZERO),
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::ZERO,
    };
    let conn = EtcdConnection::new("http://127.0.0.1:2379");
    let err = match etcd_sub(&g, params.run_mode, conn, "/x/") {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("etcd_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// The sink connects lazily on the first PUT, so wiring succeeds and an
/// unreachable etcd endpoint aborts the *run* rather than failing silently.
#[test]
fn pub_connection_refused_surfaces_an_error() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![EtcdEntry {
        key: "/x/k".to_string(),
        value: b"v".to_vec(),
    }]);
    let conn = EtcdConnection::new("http://127.0.0.1:59999");

    let _sink = source
        .etcd_pub(conn, None, true)
        .expect("wiring must succeed (connect is deferred to the run)");
    let outcome = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        outcome.is_err(),
        "an unreachable etcd endpoint must abort the run"
    );
}
