//! Wiring-level tests for the `web` adapter — **tier 1**: fast, and nothing
//! ever connects.
//!
//! These cover the parts that need no client on the other end: the bind
//! succeeding or failing, the historical-mode contracts (`web_sub` must not
//! block a replay; a `start_historical()` server no-ops both directions), and
//! the synchronous TLS-config error. They run in CI's ordinary `test` job.
//!
//! ```sh
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features web
//! ```
//!
//! The socket-level suite — real `tokio-tungstenite` clients, publish/subscribe
//! round trips, the `Complete` marker, TLS — lives in `web_integration.rs`,
//! because CI's `test` job filters on `not binary(/_integration$/)` and those
//! tests are too slow and too load-sensitive to run there. Add a test that
//! connects a client to *that* file, not this one.
#![cfg(feature = "web")]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wingfoil::adapters::web::{WebServer, WebSinkOps, web_sub};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UiClick {
    button: String,
    count: u32,
}

#[test]
fn bind_port_zero() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    assert!(server.port() > 0, "expected OS to assign a real port");
    drop(server);
    Ok(())
}

#[test]
fn bind_fails_when_port_occupied() -> anyhow::Result<()> {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = occupied.local_addr()?.port();
    let result = WebServer::bind(format!("127.0.0.1:{port}")).start();
    assert!(result.is_err(), "expected bind error when port occupied");
    Ok(())
}

/// A live (`start()`) server driving a graph in `RunMode::HistoricalFrom` must
/// run to completion even when a `web_sub` node is present: live browser input
/// has no place in a deterministic replay, so `web_sub` yields an empty source
/// rather than blocking the run waiting for frames.
#[test]
fn historical_web_sub_does_not_block() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    assert!(!server.is_historical_noop());

    let g = GraphBuilder::new();
    let _pub = g
        .ticker(Duration::from_millis(10))
        .count()
        .web_pub(&server, "out")?;
    let collected = web_sub::<UiClick>(&g, &server, "in")?.collapse_accumulate();

    // If `web_sub` blocked on its listener this would hang; a bounded `RunFor`
    // plus a wall-clock guard turns a regression into a failure.
    let mut runner = g.build();
    let start = Instant::now();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(20))?;
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "historical run with web_sub should not block"
    );
    assert!(
        runner.value(&collected).is_empty(),
        "web_sub must not tick in historical mode"
    );
    Ok(())
}

#[test]
fn historical_mode_is_noop() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start_historical()?;
    assert!(server.is_historical_noop());
    assert_eq!(server.port(), 0);

    let g = GraphBuilder::new();
    let _pub = g.constant(7u32).web_pub(&server, "x")?;
    let collected = web_sub::<u32>(&g, &server, "y")?.collapse_accumulate();
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))?;
    assert!(
        runner.value(&collected).is_empty(),
        "historical sub should not tick"
    );
    Ok(())
}

#[cfg(feature = "web-tls")]
#[test]
fn tls_missing_cert_surfaces_synchronously() {
    let result = WebServer::bind("127.0.0.1:0")
        .tls("/no/such/cert.pem", "/no/such/key.pem")
        .start();
    assert!(
        result.is_err(),
        "expected start() to fail when TLS cert is missing"
    );
}
