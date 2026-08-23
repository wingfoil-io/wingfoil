//! etcd adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/etcd_integration.rs` behind
//! the `etcd-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil --features etcd
//! ```
#![cfg(feature = "etcd")]

use std::time::Duration;

use wingfoil::adapters::etcd::{EtcdConnection, EtcdEntry, EtcdPubOptions, EtcdSinkOps, etcd_sub};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// An unreachable endpoint must abort the source's run rather than hang or panic
/// — the parity of legacy `test_connection_refused`.
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
    let source = g.constant(burst![EtcdEntry::new("/x/k", b"v".to_vec())]);
    let conn = EtcdConnection::new("http://127.0.0.1:59999");

    let _sink = source
        .etcd_pub(conn)
        .expect("wiring must succeed (connect is deferred to the run)");
    let outcome = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        outcome.is_err(),
        "an unreachable etcd endpoint must abort the run"
    );
}

/// The options struct must actually reach the sink. Without a service the
/// observable part is the *wiring* contract: non-default options wire without
/// error (nothing connects, nothing is leased, at graph construction) and the
/// run then aborts on the unreachable endpoint — i.e. the lease grant is
/// deferred to the first write exactly like the default path's connect.
#[test]
fn pub_with_options_defers_lease_and_conditional_write_to_the_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![EtcdEntry::new("/x/k", b"v".to_vec())]);
    let conn = EtcdConnection::new("http://127.0.0.1:59999");

    let _sink = source
        .etcd_pub_with_options(
            conn,
            EtcdPubOptions::default()
                .with_lease_ttl(Duration::from_secs(30))
                .with_force(false),
        )
        .expect("wiring must succeed (connect + lease_grant are deferred to the run)");
    let outcome = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        outcome.is_err(),
        "an unreachable etcd endpoint must abort the run"
    );
}

/// The single-entry convenience impl forwards its options to the burst impl, so
/// a `Stream<EtcdEntry>` caller never has to wrap a value in a burst to name a
/// lease.
#[test]
fn pub_single_entry_impl_accepts_options() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source: Stream<EtcdEntry> = g.constant(EtcdEntry::new("/x/k", b"v".to_vec()));
    let conn = EtcdConnection::new("http://127.0.0.1:59999");

    let _sink = source
        .etcd_pub_with_options(
            conn,
            EtcdPubOptions::default().with_lease_ttl(Duration::from_secs(30)),
        )
        .expect("wiring must succeed");
    let outcome = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        outcome.is_err(),
        "an unreachable etcd endpoint must abort the run"
    );
}
