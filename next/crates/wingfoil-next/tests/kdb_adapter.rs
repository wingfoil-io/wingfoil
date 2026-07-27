//! kdb adapter tests that need no running KDB+ service.
//!
//! The live-service parity tests live in `tests/kdb_integration.rs` behind the
//! `kdb-integration-test` feature (they probe an externally-provided `q -p 5000`
//! and skip when unreachable). Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features kdb
//! ```
#![cfg(feature = "kdb")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::kdb::{
    K, KdbConnection, KdbDeserialize, KdbError, KdbSerialize, KdbSinkOps, Row, SymbolInterner,
    kdb_read, kdb_read_cached, kdb_sub,
};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

// A trivial record: `kdb_read`/`kdb_sub` only need `KdbDeserialize`.
#[derive(Debug, Clone, Default, PartialEq)]
struct TestTrade {
    sym: String,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTrade {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((
            time,
            TestTrade {
                sym: row.get_sym(1, interner)?.to_string(),
                price: row.get(2)?.get_float()?,
                qty: row.get(3)?.get_long()?,
            },
        ))
    }
}

impl KdbSerialize for TestTrade {
    fn to_kdb_row(&self) -> K {
        K::new_compound_list(vec![
            K::new_symbol(self.sym.clone()),
            K::new_float(self.price),
            K::new_long(self.qty),
        ])
    }
}

const HOUR: Duration = Duration::from_secs(3600);

fn read_query((t0, t1): (NanoTime, NanoTime), _date: i32, _iter: usize) -> String {
    format!(
        "select from trades where time >= (`timestamp$){}j, time < (`timestamp$){}j",
        t0.to_kdb_timestamp(),
        t1.to_kdb_timestamp(),
    )
}

fn historical(start: NanoTime, secs: u64) -> RunParams {
    RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(Duration::from_secs(secs)),
        start_time: start,
    }
}

// ---- kdb_read / kdb_read_cached: params validation (no connection needed) ----

/// A RealTime run has no explicit historical start (start resolves to ZERO), so
/// the shared slicer validator rejects it before any connection is attempted.
#[test]
fn read_rejects_realtime_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(HOUR),
        start_time: NanoTime::ZERO,
    };
    let err = kdb_read::<TestTrade>(
        &g,
        params,
        KdbConnection::new("127.0.0.1", 1),
        HOUR,
        read_query,
        None,
    )
    .err()
    .expect("RealTime must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("kdb_read"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("NanoTime::ZERO"),
        "explains historical replay is required: {msg}"
    );
}

/// `RunFor::Forever` would generate an unbounded number of slices — rejected.
#[test]
fn read_rejects_forever() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        run_for: RunFor::Forever,
        start_time: NanoTime::from_kdb_timestamp(0),
    };
    let err = kdb_read::<TestTrade>(
        &g,
        params,
        KdbConnection::new("127.0.0.1", 1),
        HOUR,
        read_query,
        None,
    )
    .err()
    .expect("Forever must be rejected");
    assert!(format!("{err:#}").contains("RunFor::Forever"));
}

/// `RunFor::Cycles` provides no end time — rejected.
#[test]
fn read_rejects_cycles() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::from_kdb_timestamp(0),
    };
    let err = kdb_read::<TestTrade>(
        &g,
        params,
        KdbConnection::new("127.0.0.1", 1),
        HOUR,
        read_query,
        None,
    )
    .err()
    .expect("Cycles must be rejected");
    assert!(format!("{err:#}").contains("RunFor::Cycles"));
}

/// `kdb_read` connects at the start of the run (not wiring), so an unreachable
/// endpoint aborts the *run* — and the error context must never leak the password.
#[test]
fn read_connection_refused_aborts_run_without_leaking_password() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = KdbConnection::new("127.0.0.1", 1).with_credentials("user", "s3cr3t");
    // Wiring is a pure validate + slice — no I/O, so it succeeds.
    let _stream = kdb_read::<TestTrade>(
        &g,
        historical(NanoTime::from_kdb_timestamp(0), 86400),
        conn,
        HOUR,
        read_query,
        None,
    )
    .expect("wiring must succeed (connect is deferred to the run)");
    let err = g
        .build()
        .run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(Duration::from_secs(86400)),
        )
        .expect_err("an unreachable endpoint must abort the run");
    let msg = format!("{err:#}");
    assert!(!msg.contains("s3cr3t"), "password leaked in error: {msg}");
    assert!(msg.contains("kdb_read"), "names the adapter: {msg}");
}

/// The cached reader validates the run window the same way.
#[test]
fn read_cached_rejects_realtime_mode() {
    use wingfoil_next::adapters::kdb::CacheConfig;
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(HOUR),
        start_time: NanoTime::ZERO,
    };
    let cache = CacheConfig::new(unique_dir("kdb-cache-realtime"), u64::MAX);
    let err = kdb_read_cached::<TestTradeSerde>(
        &g,
        params,
        KdbConnection::new("127.0.0.1", 1),
        HOUR,
        cache,
        read_query,
    )
    .err()
    .expect("RealTime must be rejected");
    assert!(format!("{err:#}").contains("kdb_read_cached"));
}

// ---- kdb_sub: historical rejection at wiring, deferred connect ----

/// `kdb_sub` rejects a `HistoricalFrom` run at wiring — the live tickerplant tail
/// has no historical timeline to replay (use `kdb_read`).
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let err = kdb_sub::<TestTrade>(
        &g,
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        KdbConnection::new("127.0.0.1", 1),
        "trades",
        "`",
    )
    .err()
    .expect("HistoricalFrom must be rejected at wiring");
    let msg = format!("{err:#}");
    assert!(msg.contains("kdb_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

/// Realtime wiring is pure (no I/O); the connect failure surfaces during the run
/// and never leaks the password.
#[test]
fn sub_connection_refused_aborts_run_without_leaking_password() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = KdbConnection::new("127.0.0.1", 1).with_credentials("user", "topsecret");
    let _stream = kdb_sub::<TestTrade>(&g, RunMode::RealTime, conn, "trades", "`")
        .expect("realtime kdb_sub wires without error");
    let err = g
        .build()
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(300)),
        )
        .expect_err("an unreachable endpoint must abort the run");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("topsecret"),
        "password leaked in error: {msg}"
    );
    assert!(msg.contains("kdb_sub"), "names the adapter: {msg}");
}

// ---- kdb_write: deferred connect surfaces a failure without leaking creds ----

/// The sink connects lazily on the first write, so wiring succeeds and an
/// unreachable endpoint aborts the *run* — with no password in the error.
#[test]
fn write_connection_refused_aborts_run_without_leaking_password() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![TestTrade {
        sym: "T".into(),
        price: 1.0,
        qty: 1,
    }]);
    let conn = KdbConnection::new("127.0.0.1", 1).with_credentials("user", "hunter2");
    let _sink = source
        .kdb_write(conn, "trades", None)
        .expect("wiring must succeed (connect is deferred to the run)");
    let err = g
        .build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .expect_err("an unreachable endpoint must abort the run");
    let msg = format!("{err:#}");
    assert!(!msg.contains("hunter2"), "password leaked in error: {msg}");
    assert!(msg.contains("kdb_write"), "names the adapter: {msg}");
}

// ---- Engine-constraint tests (why kdb_read clamps to the run window) ----
//
// These use `replay_results` — the same channel primitive `kdb_read` feeds — to
// pin the monotonic-clock hazards the per-slice `WindowFilter` clamp guards
// against, without needing a live KDB instance.

/// A value whose time is before `start_time` aborts a historical run (the graph
/// clock starts at `start_time` and is monotonic). `kdb_read` avoids this by
/// dropping such rows; this pins why unclamped rows are unsafe to emit.
#[test]
fn row_before_start_time_aborts_run() {
    let g = GraphBuilder::new();
    let start = NanoTime::new(1_000_000_000_000);
    let before = NanoTime::new(u64::from(start) - 1);
    let rows = vec![Ok((1u32, before)), Ok((2u32, start))];
    let _s = g.replay_results::<u32, _>(rows).collapse().accumulate();
    let result = g.build().run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(1)),
    );
    result.expect_err("a row before start_time must abort the run");
}

/// A row *ahead* of its window advances the clock, so a later in-window row is
/// then behind the clock and aborts the run. `kdb_read`'s upper-bound clamp
/// prevents the forward jump; this pins the hazard.
#[test]
fn row_ahead_then_in_window_aborts_run() {
    let g = GraphBuilder::new();
    let start = NanoTime::new(1_000_000_000_000);
    let ahead = NanoTime::new(u64::from(start) + 100);
    let in_window = NanoTime::new(u64::from(start) + 30);
    // Non-decreasing sends are required by the channel; deliver `ahead` first,
    // then `in_window` cannot be scheduled behind it — the engine rejects it.
    let rows = vec![Ok((1u32, ahead)), Ok((2u32, in_window))];
    let _s = g.replay_results::<u32, _>(rows).collapse().accumulate();
    let result = g.build().run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(1)),
    );
    result.expect_err("a row behind the advanced clock must abort the run");
}

// ---- helpers ----

use std::sync::atomic::{AtomicU64, Ordering};

/// A unique temp dir per test invocation (pid + counter) so parallel tests never
/// collide on the same cache folder.
fn unique_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("wingfoil-next-{tag}-{}-{n}", std::process::id()))
}

// `kdb_read_cached` requires `Serialize + Deserialize + Sync`; a separate record
// carries those bounds (the other tests use the non-serde `TestTrade`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TestTradeSerde {
    sym: String,
    price: f64,
}

impl KdbDeserialize for TestTradeSerde {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((
            time,
            TestTradeSerde {
                sym: row.get_sym(1, interner)?.to_string(),
                price: row.get(2)?.get_float()?,
            },
        ))
    }
}
