//! Integration tests for the PostgreSQL adapter.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test --features postgres-integration-test -p wingfoil \
//!   -- --test-threads=1 postgres::integration_tests
//! ```

use super::*;
use crate::ValueAt;
use crate::nodes::{NodeOperators, StreamOperators, constant, produce_async};
use crate::types::{Burst, Element, NanoTime, Stream};
use crate::{RunFor, RunMode, burst};
use std::rc::Rc;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
use tokio_postgres::NoTls;

const PG_PORT: u16 = 5432;
const PG_IMAGE: &str = "postgres";
const PG_TAG: &str = "16-alpine";

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

/// Start a postgres container and return (guard, connection string).
/// Hold the returned guard for the duration of the test.
fn start_postgres() -> anyhow::Result<(impl Drop, PostgresConnection)> {
    let container = GenericImage::new(PG_IMAGE, PG_TAG)
        // The official image logs this line to stderr once initdb has finished and
        // the *real* server is accepting connections (the transient init server logs
        // to stdout, so waiting on stderr avoids connecting mid-initialisation).
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

/// One-day historical read with hourly slices.
fn read_trades(conn: PostgresConnection) -> Rc<dyn Stream<Burst<TestTrade>>> {
    postgres_read::<TestTrade>(
        conn,
        std::time::Duration::from_secs(3600),
        |(t0, t1), _date, _iter| {
            format!(
                "SELECT time, sym, price, qty FROM trades \
                 WHERE time >= '{}' AND time < '{}' ORDER BY time",
                postgres_timestamp(t0),
                postgres_timestamp(t1),
            )
        },
    )
}

fn collect_read<T: Element + Send>(
    stream: Rc<dyn Stream<Burst<T>>>,
) -> anyhow::Result<Vec<ValueAt<T>>> {
    let collected = stream.collapse().collect();
    collected.clone().run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    )?;
    Ok(collected.peek_value().to_vec())
}

// ---- Tests ----

#[test]
fn test_connection_refused() {
    let conn = PostgresConnection::new(
        "host=127.0.0.1 port=59999 user=postgres dbname=postgres connect_timeout=2",
    );
    let result = read_trades(conn).collapse().collect().run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    );
    assert!(result.is_err(), "expected connection error");
}

#[test]
fn test_read_time_sliced() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (_container, conn) = start_postgres()?;
    seed_trades(&conn, 5)?; // rows at 00:00..04:00

    let rows = collect_read(read_trades(conn))?;
    assert_eq!(rows.len(), 5, "should read all 5 rows across hourly slices");
    // Rows arrive time-ordered; first is SYM0 at 2000-01-01 00:00.
    assert_eq!(rows[0].value.sym, "SYM0");
    assert_eq!(rows[4].value.sym, "SYM4");
    Ok(())
}

/// `timestamptz` columns exercise the `DateTime<Utc>` branch of `get_nanotime`
/// (every other read test uses `timestamp`, which takes the `NaiveDateTime` branch).
/// Rows seeded at explicit UTC instants must read back at the same `NanoTime`.
#[test]
fn test_read_timestamptz() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
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

    let rows = collect_read(read_trades(conn))?;
    assert_eq!(rows.len(), 3, "should read all 3 timestamptz rows");
    assert_eq!(rows[0].value.sym, "SYM0");
    assert_eq!(rows[2].value.sym, "SYM2");
    // 2000-01-01 00:00:00 UTC is the KDB epoch; the tz column must convert to it exactly.
    assert_eq!(rows[0].time, NanoTime::from_kdb_timestamp(0));
    Ok(())
}

/// Regression: an unaligned `start_time` makes the first slice's period-aligned `t0`
/// precede it, so the caller's `time >= t0` filter over-reads rows before the run
/// window. Those must be dropped (with a warning), not emitted — emitting a pre-start
/// row aborts the historical run. Pins the fix ported from the KDB adapter.
#[test]
fn test_read_drops_rows_before_start() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
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
    let stream = read_trades(conn);
    let collected = stream.collapse().collect();
    // Run window [00:30, 01:30). Without the clamp, emitting the 00:15 row would abort
    // this run; the `?` below would surface that as a test failure.
    collected.clone().run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(std::time::Duration::from_secs(3600)),
    )?;

    let syms: Vec<String> = collected
        .peek_value()
        .iter()
        .map(|v| v.value.sym.clone())
        .collect();
    assert_eq!(
        syms,
        vec!["INWIN"],
        "pre-start row must be dropped, in-window row kept"
    );
    Ok(())
}

#[test]
fn test_read_empty_table() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &["CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)"],
    )?;

    let rows = collect_read(read_trades(conn))?;
    assert_eq!(rows.len(), 0, "empty table should yield 0 rows");
    Ok(())
}

#[test]
fn test_write_round_trip() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &["CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)"],
    )?;

    // Emit three trades at distinct hourly timestamps via the graph, then write them.
    let write_conn = conn.clone();
    let producer = produce_async(move |_ctx| async move {
        Ok(async_stream::stream! {
            for i in 0..3i64 {
                let time = NanoTime::from_kdb_timestamp(i * 3_600_000_000_000);
                yield Ok((time, TestTrade { sym: format!("W{i}"), price: 10.0 + i as f64, qty: i }));
            }
        })
    });
    postgres_write(write_conn, "trades", &producer)
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    // Verify via a direct count, then read back through the adapter.
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM trades")?, 3);

    let rows = collect_read(read_trades(conn))?;
    assert_eq!(rows.len(), 3, "should read back 3 written rows");
    assert_eq!(rows[0].value.sym, "W0");
    assert!((rows[0].value.price - 10.0).abs() < 1e-9);
    assert_eq!(rows[2].value.qty, 2);
    Ok(())
}

/// Live tail with `start_from` in the past: seeded rows arrive via the initial
/// catch-up query (no trigger needed), then rows inserted mid-run arrive via
/// NOTIFY wake-ups.
#[test]
fn test_sub_catch_up_then_live_inserts() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &[
            "CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)",
            &postgres_notify_trigger_sql("trades", "trades_feed"),
            // Pre-seeded rows, picked up by the catch-up query. Timestamps are
            // strictly after start_from — the cursor query is `time > cursor`,
            // so a row ON the cursor is (correctly) excluded.
            "INSERT INTO trades VALUES
               ('2000-01-01 00:00:01', 'SEED1', 1.0, 1),
               ('2000-01-01 00:00:02', 'SEED2', 2.0, 2)",
        ],
    )?;

    // Insert two live rows (one statement each → two NOTIFYs) shortly after the
    // subscriber has started. Timestamps must exceed the catch-up cursor.
    let insert_conn = conn.clone();
    let inserter = std::thread::spawn(move || -> anyhow::Result<()> {
        std::thread::sleep(std::time::Duration::from_millis(500));
        exec(
            &insert_conn,
            &[
                "INSERT INTO trades VALUES ('2000-01-01 00:00:03', 'LIVE3', 3.0, 3)",
                "INSERT INTO trades VALUES ('2000-01-01 00:00:04', 'LIVE4', 4.0, 4)",
            ],
        )
    });

    let stream = postgres_sub::<TestTrade, _>(
        conn,
        "trades_feed",
        NanoTime::from_kdb_timestamp(0), // cursor in the past → catch-up
        |cursor| {
            format!(
                "SELECT time, sym, price, qty FROM trades \
                 WHERE time > '{}' ORDER BY time",
                postgres_timestamp(cursor),
            )
        },
    );
    // Collect whole bursts: in real-time mode several rows can arrive in one
    // graph cycle, and `collapse()` would keep only the last of each burst.
    let collected = stream.collect();
    collected.clone().run(
        RunMode::RealTime,
        RunFor::Duration(std::time::Duration::from_secs(3)),
    )?;
    inserter.join().expect("inserter thread panicked")?;

    let syms: Vec<String> = collected
        .peek_value()
        .iter()
        .flat_map(|tick| tick.value.iter().map(|t| t.sym.clone()))
        .collect();
    assert_eq!(
        syms,
        vec!["SEED1", "SEED2", "LIVE3", "LIVE4"],
        "catch-up rows then live rows, in time order"
    );
    Ok(())
}

#[test]
fn test_write_burst_multi_row() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (_container, conn) = start_postgres()?;
    exec(
        &conn,
        &["CREATE TABLE trades (time timestamp, sym text, price float8, qty int8)"],
    )?;

    // A single burst with two records — both share the graph timestamp.
    constant(burst![
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
    .postgres_write(conn.clone(), "trades")
    .run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Cycles(1),
    )?;

    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM trades")?, 2);
    Ok(())
}
