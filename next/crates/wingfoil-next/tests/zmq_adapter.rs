//! zmq adapter tests that need no live socket peer or external service.
//!
//! The real-socket parity tests live in `tests/zmq_integration.rs` behind the
//! `zmq-integration-test` feature; the etcd service-discovery parity tests in
//! `tests/zmq_etcd_integration.rs` behind `zmq-etcd-integration-test`. Run these
//! with:
//! ```sh
//! cargo test -p wingfoil-next --features zmq
//! ```
#![cfg(feature = "zmq")]

use std::time::Duration;

use wingfoil_next::adapters::zmq::{ZeroMqPub, zmq_sub};
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

/// `zmq_sub` rejects a `HistoricalFrom` run at wiring time — the live,
/// never-closing subscriber has no historical timeline to replay, and next's
/// channel receiver would block-collect it up front and deadlock at `start`.
/// The error must name the adapter rather than hang.
#[test]
fn sub_rejects_historical_mode() {
    let g = GraphBuilder::new();
    let err = match zmq_sub::<u64>(
        &g,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        "tcp://127.0.0.1:5701",
    ) {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("zmq_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// The publisher checks the run mode at graph `start()` (like classic's
/// `start`), so a `HistoricalFrom` run aborts with a "real-time" error — parity
/// of classic `zmq_pub_historical_mode_fails`.
#[test]
fn pub_rejects_historical_mode() {
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|n: &u64| format!("{n}").into_bytes())
        .zmq_pub(5702, ());
    let result = g
        .build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));
    let err = result.expect_err("expected historical mode to fail for zmq pub");
    assert!(
        format!("{err:#}").contains("real-time"),
        "expected the error to mention real-time, got: {err:#}"
    );
}
