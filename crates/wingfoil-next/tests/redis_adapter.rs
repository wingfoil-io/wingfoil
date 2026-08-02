//! redis adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/redis_integration.rs` behind
//! the `redis-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features redis
//! ```
#![cfg(feature = "redis")]

use wingfoil_next::adapters::redis::{
    RedisConnection, RedisEntry, RedisSinkOps, RedisStreamRecord, RedisStreamSinkOps,
    redis_stream_read, redis_sub,
};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

/// A realtime run description (start time is ignored in realtime).
fn realtime(cycles: u32) -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Cycles(cycles),
        start_time: NanoTime::ZERO,
    }
}

/// A historical run description (its start time is validated against the run).
fn historical() -> RunParams {
    RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::ZERO),
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::ZERO,
    }
}

/// An unreachable endpoint must abort the Pub/Sub source's run rather than hang
/// or panic — the parity of legacy `test_connection_refused`.
#[test]
fn sub_connection_refused_aborts_the_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = RedisConnection::new("redis://127.0.0.1:59999");
    let _events = redis_sub(&g, realtime(1).run_mode, conn, "ch")
        .expect("realtime redis_sub wires without error")
        .collapse_accumulate();

    let mut runner = g.build();
    let result = runner.run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        result.is_err(),
        "an unreachable Redis endpoint must abort the run"
    );
}

/// The same for the stream source.
#[test]
fn stream_read_connection_refused_aborts_the_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = RedisConnection::new("redis://127.0.0.1:59999");
    let _events = redis_stream_read(&g, realtime(1).run_mode, conn, "key")
        .expect("realtime redis_stream_read wires without error")
        .collapse_accumulate();

    let mut runner = g.build();
    let result = runner.run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        result.is_err(),
        "an unreachable Redis endpoint must abort the run"
    );
}

/// `redis_sub` rejects a `HistoricalFrom` run at wiring time — the live,
/// unbounded, wall-clock Pub/Sub stream has no historical timeline to replay.
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = RedisConnection::new("redis://127.0.0.1:6379");
    let err = match redis_sub(&g, historical().run_mode, conn, "ch") {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("redis_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// `redis_stream_read` likewise rejects a `HistoricalFrom` run at wiring time.
#[test]
fn stream_read_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = RedisConnection::new("redis://127.0.0.1:6379");
    let err = match redis_stream_read(&g, historical().run_mode, conn, "key") {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("redis_stream_read"),
        "names the adapter: {msg}"
    );
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// The Pub/Sub sink connects lazily on the first write, so wiring succeeds and
/// an unreachable endpoint aborts the *run*.
#[test]
fn pub_connection_refused_surfaces_an_error() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![RedisEntry {
        channel: "ch".to_string(),
        payload: b"v".to_vec(),
    }]);
    let conn = RedisConnection::new("redis://127.0.0.1:59999");

    let _sink = source
        .redis_pub(conn, None)
        .expect("wiring must succeed (connect is deferred to the run)");
    let outcome = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        outcome.is_err(),
        "an unreachable Redis endpoint must abort the run"
    );
}

/// A connection URL can embed a password (`redis://user:pass@host`). When the
/// deferred connect fails at run time, the error context must **redact** that
/// password — it must never reach a log or the aborted-run error (parity with
/// the postgres adapter; a leak regression versus legacy that this restores).
#[test]
fn pub_connection_refused_redacts_password() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![RedisEntry {
        channel: "ch".to_string(),
        payload: b"v".to_vec(),
    }]);
    let conn = RedisConnection::new("redis://user:sup3rs3cr3t@127.0.0.1:59999/0");

    let _sink = source
        .redis_pub(conn, None)
        .expect("wiring must succeed (connect is deferred to the run)");
    let err = g
        .build()
        .run(RunMode::RealTime, RunFor::Cycles(1))
        .expect_err("an unreachable Redis endpoint must abort the run");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("sup3rs3cr3t"),
        "password leaked in error: {msg}"
    );
    assert!(msg.contains("***:***"), "userinfo not redacted: {msg}");
}

/// The stream sink connects lazily the same way.
#[test]
fn stream_write_connection_refused_surfaces_an_error() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![RedisStreamRecord::single("key", "f", b"v".to_vec())]);
    let conn = RedisConnection::new("redis://127.0.0.1:59999");

    let _sink = source
        .redis_stream_write(conn, None)
        .expect("wiring must succeed (connect is deferred to the run)");
    let outcome = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(
        outcome.is_err(),
        "an unreachable Redis endpoint must abort the run"
    );
}
