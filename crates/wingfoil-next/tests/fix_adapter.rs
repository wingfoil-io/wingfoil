//! Wiring-level parity tests for the FIX adapter that need no live peer:
//! historical-mode rejection at wiring (all source factories + both poll modes)
//! and the `fix_send` sink's realtime-only check at run start. The codec /
//! session / `FixSender`-queue unit tests live inline in the module
//! (`src/adapters/fix.rs`); the same-process socket round-trip + reconnect
//! parity tests live in `tests/fix_integration.rs` (feature
//! `fix-integration-test`).
#![cfg(feature = "fix")]

use wingfoil_next::adapters::fix::{
    FixMessage, FixOperators, FixPollMode, fix_accept, fix_connect, fix_connect_tls,
};
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Every FIX source factory rejects `RunMode::HistoricalFrom` at wiring time
/// with a "real-time" error — a live session has no historical timeline to
/// replay. Ports classic's `fix_historical_mode_fails` (which caught it at run
/// start), moved earlier to wiring per the next live-source convention.
#[test]
fn sources_reject_historical_at_wiring() {
    for mode in [FixPollMode::AlwaysSpin, FixPollMode::Threaded] {
        let g = GraphBuilder::new();
        let err = fix_connect(&g, HISTORICAL, "127.0.0.1", 1, "S", "T", mode)
            .map(|_| ())
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("real-time"),
            "fix_connect: expected real-time error, got: {err:?}"
        );
    }

    for mode in [FixPollMode::AlwaysSpin, FixPollMode::Threaded] {
        let g = GraphBuilder::new();
        let err = fix_accept(&g, HISTORICAL, 1, "S", "T", mode)
            .map(|_| ())
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("real-time"),
            "fix_accept: expected real-time error, got: {err:?}"
        );
    }

    let g = GraphBuilder::new();
    let err = fix_connect_tls(&g, HISTORICAL, "127.0.0.1", 1, "S", "T", Some("pw"))
        .map(|_| ())
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("real-time"),
        "fix_connect_tls: expected real-time error, got: {err:?}"
    );
}

/// The `fix_send` sink is realtime-only: a historical run aborts at graph
/// `start()` with a "real-time" error (classic's `FixSenderNode::start` check),
/// before any socket connect. Uses a dead port to prove no connection is
/// attempted — the run fails on the run-mode check, not a connect error.
#[test]
fn fix_send_rejects_historical_at_start() {
    let g = GraphBuilder::new();
    let _sink = g
        .constant(FixMessage::default())
        .fix_send("127.0.0.1", 1, "SENDER", "TARGET")
        .expect("fix_send wiring succeeds");

    let err = g
        .build()
        .run(HISTORICAL, RunFor::Cycles(1))
        .expect_err("expected historical mode to fail at start");
    assert!(
        format!("{err:?}").contains("real-time"),
        "expected real-time error, got: {err:?}"
    );
}
