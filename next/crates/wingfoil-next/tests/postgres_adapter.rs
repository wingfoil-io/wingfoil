//! postgres adapter tests that need no running service.
//!
//! The container-backed parity tests live in `tests/postgres_integration.rs`
//! behind the `postgres-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features postgres
//! ```
#![cfg(feature = "postgres")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::postgres::{
    PostgresConnection, PostgresDeserialize, PostgresRowExt, PostgresSerialize, PostgresSinkOps,
    PostgresSourceConfig, Row, ToSql, postgres_read, postgres_source, postgres_sub,
};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

#[derive(Debug, Clone, Default, PartialEq)]
struct TestTrade {
    sym: String,
    price: f64,
    qty: i64,
}

impl PostgresDeserialize for TestTrade {
    fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)> {
        Ok((
            row.get_nanotime(0)?,
            TestTrade {
                sym: row.try_get(1)?,
                price: row.try_get(2)?,
                qty: row.try_get(3)?,
            },
        ))
    }
}

impl PostgresSerialize for TestTrade {
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
        vec![
            Box::new(self.sym.clone()),
            Box::new(self.price),
            Box::new(self.qty),
        ]
    }
}

const HOUR: Duration = Duration::from_secs(3600);

/// A time-sliced hourly read query closure.
fn read_query((t0, t1): (NanoTime, NanoTime), _date: i32, _iter: usize) -> String {
    use wingfoil_next::adapters::postgres::postgres_timestamp;
    format!(
        "SELECT time, sym, price, qty FROM trades WHERE time >= '{}' AND time < '{}' ORDER BY time",
        postgres_timestamp(t0),
        postgres_timestamp(t1),
    )
}

fn historical(start: NanoTime, secs: u64) -> RunParams {
    RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(Duration::from_secs(secs)),
        start_time: start,
    }
}

// ---- postgres_read: params validation (no connection needed) ----

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
    let err = postgres_read::<TestTrade>(&g, params, "host=127.0.0.1 dbname=db", HOUR, read_query)
        .err()
        .expect("RealTime must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("postgres_read"), "names the adapter: {msg}");
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
    let err = postgres_read::<TestTrade>(&g, params, "host=127.0.0.1 dbname=db", HOUR, read_query)
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
    let err = postgres_read::<TestTrade>(&g, params, "host=127.0.0.1 dbname=db", HOUR, read_query)
        .err()
        .expect("Cycles must be rejected");
    assert!(format!("{err:#}").contains("RunFor::Cycles"));
}

/// An unreachable endpoint aborts `postgres_read` at wiring time, and the error
/// context must **redact** the password (parity with classic PR #433).
#[test]
fn read_connection_refused_redacts_password() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let conn = PostgresConnection::new(
        "host=127.0.0.1 port=59999 user=postgres password=s3cr3t dbname=postgres connect_timeout=2",
    );
    let err = postgres_read::<TestTrade>(
        &g,
        historical(NanoTime::from_kdb_timestamp(0), 86400),
        conn,
        HOUR,
        read_query,
    )
    .err()
    .expect("an unreachable endpoint must abort at wiring");
    let msg = format!("{err:#}");
    assert!(!msg.contains("s3cr3t"), "password leaked in error: {msg}");
    assert!(msg.contains("password=***"), "password not redacted: {msg}");
}

// ---- postgres_sub: historical rejection at wiring ----

/// `postgres_sub` rejects a `HistoricalFrom` run at wiring — the live tail has no
/// historical timeline to replay (use `postgres_read`).
#[test]
fn sub_rejects_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        run_for: RunFor::Cycles(1),
        start_time: NanoTime::from_kdb_timestamp(0),
    };
    let err = match postgres_sub::<TestTrade, _>(
        &g,
        params.run_mode,
        "host=127.0.0.1 dbname=db",
        "chan",
        NanoTime::ZERO,
        |_cursor| String::new(),
    ) {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("postgres_sub"), "names the adapter: {msg}");
    assert!(
        msg.contains("HistoricalFrom") || msg.contains("historical"),
        "explains historical replay is unsupported: {msg}"
    );
}

// ---- postgres_write: eager connect surfaces (and redacts) a failure ----

/// The sink connects eagerly at wiring; an unreachable endpoint surfaces an error
/// there, again with the password redacted.
#[test]
fn write_connection_refused_redacts_password() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g.constant(burst![TestTrade {
        sym: "T".into(),
        price: 1.0,
        qty: 1,
    }]);
    let conn = PostgresConnection::new(
        "host=127.0.0.1 port=59999 user=postgres password=hunter2 dbname=postgres connect_timeout=2",
    );
    let err = source
        .postgres_write(conn, "trades", None)
        .err()
        .expect("an unreachable endpoint must abort at wiring");
    let msg = format!("{err:#}");
    assert!(!msg.contains("hunter2"), "password leaked in error: {msg}");
    assert!(msg.contains("password=***"), "password not redacted: {msg}");
}

// ---- postgres_source: mode dispatch (no connection needed) ----

fn realtime() -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    }
}

/// A `HistoricalFrom` run with only a live half configured errors at wiring,
/// naming the missing historical half — before any connection is attempted.
#[test]
fn source_missing_historical_half_errors_under_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = PostgresSourceConfig::new().live("chan", NanoTime::ZERO, |_cursor| String::new());
    let err = postgres_source::<TestTrade>(
        &g,
        historical(NanoTime::from_kdb_timestamp(0), 86400),
        "host=127.0.0.1 dbname=db",
        cfg,
    )
    .err()
    .expect("HistoricalFrom without a historical config must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("postgres_source"), "names the adapter: {msg}");
    assert!(
        msg.contains("historical"),
        "names the missing historical half: {msg}"
    );
}

/// A `RealTime` run with only a historical half configured errors at wiring,
/// naming the missing live half.
#[test]
fn source_missing_live_half_errors_under_realtime() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = PostgresSourceConfig::new().historical(HOUR, read_query);
    let err = postgres_source::<TestTrade>(&g, realtime(), "host=127.0.0.1 dbname=db", cfg)
        .err()
        .expect("RealTime without a live config must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("postgres_source"), "names the adapter: {msg}");
    assert!(msg.contains("live"), "names the missing live half: {msg}");
}

/// A `HistoricalFrom` run dispatches to the `postgres_read` mechanism, so it
/// inherits that mechanism's bounded-window validation: `RunFor::Forever` (an
/// unbounded slice set) is rejected with `postgres_read`'s message — proof the
/// dispatch reached the historical primitive.
#[test]
fn source_dispatches_to_read_under_historical_mode() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = PostgresSourceConfig::new().historical(HOUR, read_query);
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        run_for: RunFor::Forever,
        start_time: NanoTime::from_kdb_timestamp(0),
    };
    let err = postgres_source::<TestTrade>(&g, params, "host=127.0.0.1 dbname=db", cfg)
        .err()
        .expect("Forever must be rejected by the read mechanism");
    assert!(
        format!("{err:#}").contains("RunFor::Forever"),
        "dispatched to postgres_read's validation: {err:#}"
    );
}

/// A `RealTime` run with a live half dispatches to the `postgres_sub` mechanism,
/// which connects lazily in its producer task — so wiring succeeds (the graph is
/// never run here). Proof the realtime dispatch reaches the live primitive.
#[test]
fn source_dispatches_to_sub_under_realtime() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = PostgresSourceConfig::new().live("chan", NanoTime::ZERO, |cursor: NanoTime| {
        format!("SELECT time, sym, price, qty FROM trades WHERE time > '{cursor:?}' ORDER BY time")
    });
    let wired = postgres_source::<TestTrade>(&g, realtime(), "host=127.0.0.1 dbname=db", cfg);
    assert!(
        wired.is_ok(),
        "RealTime with a live config wires (sub connects lazily): {:?}",
        wired.err()
    );
}
