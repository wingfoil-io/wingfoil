//! Integration tests for the postgres adapter — parity port of the classic
//! `wingfoil/src/adapters/postgres/integration_tests.rs`.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test -p wingfoil-next --features postgres-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "postgres-integration-test")]

use std::time::Duration;

use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
use tokio::runtime::Handle;
use tokio_postgres::NoTls;
use wingfoil_next::adapters::postgres::{
    PostgresConnection, PostgresDeserialize, PostgresRowExt, PostgresSerialize, PostgresSinkOps,
    PostgresSourceConfig, Row, ToSql, postgres_notify_trigger_sql, postgres_read, postgres_source,
    postgres_sub, postgres_timestamp,
};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const PG_PORT: u16 = 5432;
const PG_IMAGE: &str = "postgres";
const PG_TAG: &str = "16-alpine";
const HOUR: Duration = Duration::from_secs(3600);
const HOUR_NANOS: i64 = 3_600_000_000_000;

/// Business record for the tests. Time is on-graph, not in the struct.
#[derive(Debug, Clone, Default, PartialEq)]
struct TestTrade {
    sym: String,
    price: f64,
    qty: i64,
}

impl PostgresDeserialize for TestTrade {
    fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)> {
        Ok((
            row.get_nanotime(0)?, // col 0: time
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

/// Start a postgres container and return (guard, connection).
/// Hold the returned guard for the duration of the test.
fn start_postgres() -> anyhow::Result<(impl Drop, PostgresConnection)> {
    let container = GenericImage::new(PG_IMAGE, PG_TAG)
        // The official image logs this line to stderr once initdb has finished
        // and the *real* server is accepting connections.
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .start()?;
    let port = container.get_host_port_ipv4(PG_PORT)?;
    let conn_str = format!(
        "host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres connect_timeout=10"
    );
    Ok((container, PostgresConnection::new(conn_str)))
}

/// Run `stmt`s against postgres using a throwaway runtime.
fn exec(conn: &PostgresConnection, stmts: &[&str]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&conn.conn_str, NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        for stmt in stmts {
            client.batch_execute(stmt).await?;
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// Query a single `i64` scalar (e.g. `SELECT count(*) …`).
fn scalar_i64(conn: &PostgresConnection, query: &str) -> anyhow::Result<i64> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&conn.conn_str, NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let row = client.query_one(query, &[]).await?;
        Ok::<i64, anyhow::Error>(row.get(0))
    })
}

/// Create the trades table and seed `n` rows, one per hour from 2000-01-01 00:00.
fn seed_trades(conn: &PostgresConnection, n: usize) -> anyhow::Result<()> {
    exec(
        conn,
        &[
            "CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)",
            &format!(
                "INSERT INTO trades \
                 SELECT timestamp '2000-01-01 00:00:00' + (g || ' hours')::interval, \
                        'SYM' || g, 100.0 + g, g \
                 FROM generate_series(0, {}) AS g",
                n - 1
            ),
        ],
    )
}

/// An hourly time-sliced read query.
fn read_query((t0, t1): (NanoTime, NanoTime), _date: i32, _iter: usize) -> String {
    format!(
        "SELECT time, sym, price, qty FROM trades WHERE time >= '{}' AND time < '{}' ORDER BY time",
        postgres_timestamp(t0),
        postgres_timestamp(t1),
    )
}

/// One-day historical read (hourly slices) collected as `(time, trade)` rows.
fn collect_hourly_read(
    handle: &Handle,
    conn: PostgresConnection,
    start: NanoTime,
    dur: Duration,
) -> anyhow::Result<Vec<(NanoTime, TestTrade)>> {
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(dur),
        start_time: start,
    };
    let g = GraphBuilder::new().with_async_runtime(handle.clone());
    let acc = postgres_read::<TestTrade>(&g, params, conn, HOUR, read_query, None)?
        .collapse()
        .with_time()
        .accumulate();
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(start), RunFor::Duration(dur))?;
    Ok(runner.value(&acc))
}

// ---- Tests ----

#[test]
fn test_read_time_sliced() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    seed_trades(&conn, 5)?; // rows at 00:00..04:00

    let rows = collect_hourly_read(
        rt.handle(),
        conn,
        NanoTime::from_kdb_timestamp(0),
        Duration::from_secs(86400),
    )?;
    assert_eq!(rows.len(), 5, "should read all 5 rows across hourly slices");
    // Values are time-ordered and stamped at the row timestamps.
    let syms: Vec<String> = rows.iter().map(|(_, t)| t.sym.clone()).collect();
    assert_eq!(syms, vec!["SYM0", "SYM1", "SYM2", "SYM3", "SYM4"]);
    let times: Vec<NanoTime> = rows.iter().map(|(time, _)| *time).collect();
    let expected: Vec<NanoTime> = (0..5)
        .map(|i| NanoTime::from_kdb_timestamp(i * HOUR_NANOS))
        .collect();
    assert_eq!(times, expected, "rows must tick at their row timestamps");
    Ok(())
}

/// The mode-agnostic `postgres_source` dispatches a `HistoricalFrom` run to the
/// bounded read mechanism — identical result to `postgres_read` (the primitive it
/// wires through) for the same window.
#[test]
fn test_source_historical_dispatches_to_read() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    seed_trades(&conn, 5)?; // rows at 00:00..04:00

    let start = NanoTime::from_kdb_timestamp(0);
    let dur = Duration::from_secs(86400);
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(dur),
        start_time: start,
    };
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let cfg = PostgresSourceConfig::new().historical(HOUR, read_query, None);
    let acc = postgres_source::<TestTrade>(&g, params, conn, cfg)?
        .collapse()
        .with_time()
        .accumulate();
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(start), RunFor::Duration(dur))?;
    let rows = runner.value(&acc);

    assert_eq!(rows.len(), 5, "source (historical) reads all 5 rows");
    let syms: Vec<String> = rows.iter().map(|(_, t)| t.sym.clone()).collect();
    assert_eq!(syms, vec!["SYM0", "SYM1", "SYM2", "SYM3", "SYM4"]);
    Ok(())
}

/// `timestamptz` columns exercise the `DateTime<Utc>` branch of `get_nanotime`.
#[test]
fn test_read_timestamptz() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &[
            "CREATE TABLE trades (time timestamptz, sym text, price float8, qty int8)",
            "INSERT INTO trades \
             SELECT timestamptz '2000-01-01 00:00:00+00' + (g || ' hours')::interval, \
                    'SYM' || g, 100.0 + g, g \
             FROM generate_series(0, 2) AS g",
        ],
    )?;

    let rows = collect_hourly_read(
        rt.handle(),
        conn,
        NanoTime::from_kdb_timestamp(0),
        Duration::from_secs(86400),
    )?;
    assert_eq!(rows.len(), 3, "should read all 3 timestamptz rows");
    assert_eq!(rows[0].1.sym, "SYM0");
    assert_eq!(rows[2].1.sym, "SYM2");
    // 2000-01-01 00:00:00 UTC is the KDB epoch; the tz column must convert exactly.
    assert_eq!(rows[0].0, NanoTime::from_kdb_timestamp(0));
    Ok(())
}

/// An unaligned `start_time` over-reads pre-start rows; they must be dropped
/// (with a warning), not emitted — emitting a pre-start row aborts the run.
#[test]
fn test_read_drops_rows_before_start() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &[
            "CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)",
            // 00:15 precedes the run's 00:30 start; 00:45 is inside the window.
            "INSERT INTO trades VALUES \
               ('2000-01-01 00:15:00', 'EARLY', 1.0, 1), \
               ('2000-01-01 00:45:00', 'INWIN', 2.0, 2)",
        ],
    )?;

    // Start at 00:30 — unaligned to the 1-hour period, so the first slice's t0 is
    // 00:00 and `time >= t0` returns the 00:15 row (before start_time).
    let start = NanoTime::from_kdb_timestamp(30 * 60 * 1_000_000_000);
    let rows = collect_hourly_read(rt.handle(), conn, start, HOUR)?;
    let syms: Vec<String> = rows.iter().map(|(_, t)| t.sym.clone()).collect();
    assert_eq!(
        syms,
        vec!["INWIN"],
        "pre-start row must be dropped, in-window row kept"
    );
    Ok(())
}

#[test]
fn test_read_empty_table() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &["CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)"],
    )?;

    let rows = collect_hourly_read(
        rt.handle(),
        conn,
        NanoTime::from_kdb_timestamp(0),
        Duration::from_secs(86400),
    )?;
    assert_eq!(rows.len(), 0, "empty table should yield 0 rows");
    Ok(())
}

#[test]
fn test_write_round_trip() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &["CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)"],
    )?;

    // Emit three trades at distinct hourly timestamps via a replay source, write
    // them, then drop the runner so the off-thread sink flushes at teardown.
    let start = NanoTime::from_kdb_timestamp(0);
    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let rows = (0..3i64).map(|i| {
            Ok((
                TestTrade {
                    sym: format!("W{i}"),
                    price: 10.0 + i as f64,
                    qty: i,
                },
                NanoTime::from_kdb_timestamp(i * HOUR_NANOS),
            ))
        });
        let _sink = g
            .replay_results(rows)
            .postgres_write(conn.clone(), "trades", None)?;
        let mut runner = g.build();
        runner.run(
            RunMode::HistoricalFrom(start),
            RunFor::Duration(Duration::from_secs(4 * 3600)),
        )?;
    }

    // Verify via a direct count, then read back through the adapter.
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM trades")?, 3);

    let rows = collect_hourly_read(rt.handle(), conn, start, Duration::from_secs(86400))?;
    assert_eq!(rows.len(), 3, "should read back 3 written rows");
    assert_eq!(rows[0].1.sym, "W0");
    assert!((rows[0].1.price - 10.0).abs() < 1e-9);
    assert_eq!(rows[2].1.qty, 2);
    Ok(())
}

#[test]
fn test_write_burst_multi_row() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &["CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)"],
    )?;

    // A single burst with two records — both share the graph timestamp.
    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g
            .constant(burst![
                TestTrade {
                    sym: "A".into(),
                    price: 1.0,
                    qty: 1
                },
                TestTrade {
                    sym: "B".into(),
                    price: 2.0,
                    qty: 2
                },
            ])
            .postgres_write(conn.clone(), "trades", None)?;
        let mut runner = g.build();
        runner.run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Cycles(1),
        )?;
    }

    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM trades")?, 2);
    Ok(())
}

/// Live tail with `start_from` in the past: seeded rows arrive via the initial
/// catch-up query, then rows inserted mid-run arrive via NOTIFY wake-ups.
#[test]
fn test_sub_catch_up_then_live_inserts() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &[
            "CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)",
            &postgres_notify_trigger_sql("trades", "trades_feed"),
            // Pre-seeded rows, picked up by the catch-up query. Timestamps are
            // strictly after start_from — the cursor query is `time > cursor`.
            "INSERT INTO trades VALUES
               ('2000-01-01 00:00:01', 'SEED1', 1.0, 1),
               ('2000-01-01 00:00:02', 'SEED2', 2.0, 2)",
        ],
    )?;

    // Insert two live rows shortly after the subscriber has started.
    let insert_conn = conn.clone();
    let inserter = std::thread::spawn(move || -> anyhow::Result<()> {
        std::thread::sleep(Duration::from_millis(500));
        exec(
            &insert_conn,
            &[
                "INSERT INTO trades VALUES ('2000-01-01 00:00:03', 'LIVE3', 3.0, 3)",
                "INSERT INTO trades VALUES ('2000-01-01 00:00:04', 'LIVE4', 4.0, 4)",
            ],
        )
    });

    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(Duration::from_secs(3)),
        start_time: NanoTime::ZERO,
    };
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    // Collect whole bursts: in real-time several rows can arrive in one cycle.
    let acc = postgres_sub::<TestTrade, _>(
        &g,
        params.run_mode,
        conn,
        "trades_feed",
        NanoTime::from_kdb_timestamp(0), // cursor in the past → catch-up
        |cursor| {
            format!(
                "SELECT time, sym, price, qty FROM trades WHERE time > '{}' ORDER BY time",
                postgres_timestamp(cursor),
            )
        },
    )?
    .accumulate();
    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))?;
    inserter.join().expect("inserter thread panicked")?;

    let syms: Vec<String> = runner
        .value(&acc)
        .into_iter()
        .flat_map(|burst| burst.into_iter().map(|t| t.sym))
        .collect();
    assert_eq!(
        syms,
        vec!["SEED1", "SEED2", "LIVE3", "LIVE4"],
        "catch-up rows then live rows, in time order"
    );
    Ok(())
}
