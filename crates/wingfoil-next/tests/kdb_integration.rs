//! Integration tests for the kdb adapter — parity port of the classic
//! `wingfoil/src/adapters/kdb/{integration_tests,cache_integration_tests}.rs`.
//!
//! KDB+ has no public, freely-licensed container image, so these tests probe an
//! externally-provided q instance and **skip** (printing a notice) when it is
//! unreachable — there is no `testcontainers` dependency. Start one with:
//! ```sh
//! q -p 5000
//! ```
//! then run:
//! ```sh
//! cargo test -p wingfoil-next --features kdb-integration-test -- --test-threads=1 --nocapture
//! ```
//! Environment: `KDB_TEST_HOST` (default `localhost`), `KDB_TEST_PORT` (default 5000).
#![cfg(feature = "kdb-integration-test")]

use std::time::Duration;

use anyhow::{Context, Result};
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use wingfoil_next::adapters::kdb::{
    CacheConfig, KdbConnection, KdbDeserialize, KdbError, KdbSerialize, KdbSinkOps, Row, Sym,
    SymbolInterner, kdb_read, kdb_read_cached, kdb_sub,
};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const TABLE_NAME: &str = "test_trades";
const WRITE_TABLE_NAME: &str = "test_trades_write";

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Read table record: (date, time, sym, price, qty) — time at col 1.
#[derive(Debug, Clone, Default, PartialEq)]
struct TestTrade {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbSerialize for TestTrade {
    fn to_kdb_row(&self) -> K {
        K::new_compound_list(vec![
            K::new_symbol(self.sym.to_string()),
            K::new_float(self.price),
            K::new_long(self.qty),
        ])
    }
}

impl KdbDeserialize for TestTrade {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(1)?; // col 0: date, col 1: time
        Ok((
            time,
            TestTrade {
                sym: row.get_sym(2, interner)?,
                price: row.get(3)?.get_float()?,
                qty: row.get(4)?.get_long()?,
            },
        ))
    }
}

/// Write table record: (time, sym, price, qty) — no date column, time at col 0.
#[derive(Debug, Clone, Default, PartialEq)]
struct TestTradeWrite {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTradeWrite {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((
            time,
            TestTradeWrite {
                sym: row.get_sym(1, interner)?,
                price: row.get(2)?.get_float()?,
                qty: row.get(3)?.get_long()?,
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Raw-KDB test helpers (setup / teardown / verify)
// ---------------------------------------------------------------------------

fn test_connection() -> KdbConnection {
    let port = std::env::var("KDB_TEST_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5000);
    let host = std::env::var("KDB_TEST_HOST").unwrap_or_else(|_| "localhost".to_string());
    KdbConnection::new(host, port)
}

async fn connect(conn: &KdbConnection) -> Result<QStream> {
    let creds = conn.credentials_string();
    QStream::connect(ConnectionMethod::TCP, &conn.host, conn.port, &creds)
        .await
        .context("Failed to connect to KDB+")
}

/// Send a q expression synchronously, erroring on a q error object (type -128).
async fn q_exec(socket: &mut QStream, query: &str) -> Result<K> {
    let result = socket
        .send_sync_message(&query)
        .await
        .with_context(|| format!("failed to send `{query}`"))?;
    if result.get_type() == -128 {
        anyhow::bail!("KDB+ query error for `{query}`: {result:?}");
    }
    Ok(result)
}

/// Probe the test instance; on failure print a skip notice and return `None` so
/// the caller returns early (KDB+ is not containerisable in CI).
fn kdb_or_skip(test: &str) -> Option<(KdbConnection, tokio::runtime::Runtime)> {
    let conn = test_connection();
    let rt = tokio::runtime::Runtime::new().ok()?;
    match rt.block_on(connect(&conn)) {
        Ok(_) => Some((conn, rt)),
        Err(e) => {
            eprintln!(
                "skipping {test}: no KDB+ at {} ({e}); start `q -p {}`",
                conn.redacted(),
                conn.port
            );
            None
        }
    }
}

/// Create the read table (with a date column) and insert `records_per_day` ×
/// `num_days` sorted rows spread evenly across each day.
fn setup_read_table(
    rt: &tokio::runtime::Runtime,
    conn: &KdbConnection,
    records_per_day: usize,
    num_days: usize,
) -> Result<()> {
    rt.block_on(async {
        let mut s = connect(conn).await?;
        q_exec(
            &mut s,
            &format!(
                "{TABLE_NAME}:([]date:`date$();time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())"
            ),
        )
        .await?;
        let n = records_per_day * num_days;
        let date_expr = format!("raze {{{records_per_day}#2000.01.01+x}} each til {num_days}");
        let time_expr = format!(
            "raze {{(`timestamp$2000.01.01+x)+(86400000000000j div {records_per_day}j)*til {records_per_day}}} each til {num_days}"
        );
        q_exec(
            &mut s,
            &format!(
                "insert[`{TABLE_NAME};({date_expr};{time_expr};{n}?`AAPL`GOOG`MSFT;{n}?100.0;{n}?1000j)]"
            ),
        )
        .await?;
        Ok(())
    })
}

fn drop_table(rt: &tokio::runtime::Runtime, conn: &KdbConnection, table: &str) -> Result<()> {
    rt.block_on(async {
        let mut s = connect(conn).await?;
        q_exec(&mut s, &format!("delete {table} from `.")).await?;
        Ok(())
    })
}

fn table_count(rt: &tokio::runtime::Runtime, conn: &KdbConnection, table: &str) -> Result<i64> {
    rt.block_on(async {
        let mut s = connect(conn).await?;
        let result = q_exec(&mut s, &format!("count {table}")).await?;
        result.get_long().map_err(anyhow::Error::new)
    })
}

/// A time-slice query for `TABLE_NAME` filtering by date and half-open time range.
fn slice_query(date: i32, t0: NanoTime, t1: NanoTime) -> String {
    format!(
        "select from {} where date=2000.01.01+{}, time >= (`timestamp$){}j, time < (`timestamp$){}j",
        TABLE_NAME,
        date,
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

/// Run a `kdb_read` graph and return the (collapsed) rows.
fn read_rows<T>(
    conn: KdbConnection,
    period: Duration,
    span_secs: u64,
    query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
) -> Result<Vec<T>>
where
    T: KdbDeserialize + Clone + Default + Send + 'static,
{
    let start = NanoTime::from_kdb_timestamp(0);
    let g = GraphBuilder::new();
    let acc = kdb_read::<T>(
        &g,
        historical(start, span_secs),
        conn,
        period,
        query_fn,
        None,
    )?
    .collapse_accumulate();
    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(span_secs)),
    )?;
    Ok(runner.value(&acc))
}

// ---------------------------------------------------------------------------
// Read tests
// ---------------------------------------------------------------------------

#[test]
fn test_kdb_sorted_data() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_sorted_data") else {
        return Ok(());
    };
    setup_read_table(&rt, &conn, 3, 2)?;
    let result: Result<()> = (|| {
        let rows = read_rows::<TestTrade>(
            conn.clone(),
            Duration::from_secs(24 * 3600),
            2 * 86400,
            |w, date, _| slice_query(date, w.0, w.1),
        )?;
        assert_eq!(rows.len(), 6, "should read all 6 rows (3/day × 2 days)");
        Ok(())
    })();
    let td = drop_table(&rt, &conn, TABLE_NAME);
    result?;
    td
}

#[test]
fn test_kdb_read_works_multi_slice() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_read_works_multi_slice") else {
        return Ok(());
    };
    setup_read_table(&rt, &conn, 3, 2)?;
    let result: Result<()> = (|| {
        // 12h slices → 2 slices/day, 4 total; all 6 rows delivered.
        let rows = read_rows::<TestTrade>(
            conn.clone(),
            Duration::from_secs(12 * 3600),
            2 * 86400,
            |w, date, _| slice_query(date, w.0, w.1),
        )?;
        assert_eq!(rows.len(), 6, "should read all 6 rows across 4 slices");
        Ok(())
    })();
    let td = drop_table(&rt, &conn, TABLE_NAME);
    result?;
    td
}

#[test]
fn test_kdb_empty_table_returns_zero_rows() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_empty_table_returns_zero_rows") else {
        return Ok(());
    };
    rt.block_on(async {
        let mut s = connect(&conn).await?;
        q_exec(
            &mut s,
            &format!("{TABLE_NAME}:([]date:`date$();time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())"),
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    })?;
    let result: Result<()> = (|| {
        let rows = read_rows::<TestTrade>(
            conn.clone(),
            Duration::from_secs(24 * 3600),
            86400,
            |w, date, _| slice_query(date, w.0, w.1),
        )?;
        assert_eq!(rows.len(), 0, "empty table should return 0 rows");
        Ok(())
    })();
    let td = drop_table(&rt, &conn, TABLE_NAME);
    result?;
    td
}

#[test]
fn test_kdb_bad_query_aborts_run() -> Result<()> {
    let Some((conn, _rt)) = kdb_or_skip("test_kdb_bad_query_aborts_run") else {
        return Ok(());
    };
    let result = read_rows::<TestTrade>(conn, Duration::from_secs(24 * 3600), 86400, |_, _, _| {
        "select from nonexistent_table_xyz".to_string()
    });
    assert!(result.is_err(), "a bad query must abort the run");
    Ok(())
}

/// A struct that reads the wrong column type to force a deserialization error.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct BadTrade {
    sym: i64,
}

impl KdbDeserialize for BadTrade {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        _interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(1)?;
        Ok((
            time,
            BadTrade {
                sym: row.get(2)?.get_long()?,
            },
        )) // col 2 is a symbol
    }
}

#[test]
fn test_kdb_deserialization_error_aborts_run() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_deserialization_error_aborts_run") else {
        return Ok(());
    };
    setup_read_table(&rt, &conn, 3, 1)?;
    let result = read_rows::<BadTrade>(
        conn.clone(),
        Duration::from_secs(24 * 3600),
        86400,
        |w, date, _| slice_query(date, w.0, w.1),
    );
    let td = drop_table(&rt, &conn, TABLE_NAME);
    assert!(result.is_err(), "a type mismatch must abort the run");
    td
}

/// `kdb_read` drops rows the query returns outside the run window rather than
/// aborting (parity with classic `test_kdb_read_drops_rows_outside_window`).
#[test]
fn test_kdb_read_drops_rows_outside_window() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_read_drops_rows_outside_window") else {
        return Ok(());
    };
    const TBL: &str = "read_window_trades";
    rt.block_on(async {
        let mut s = connect(&conn).await?;
        q_exec(
            &mut s,
            &format!("{TBL}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())"),
        )
        .await?;
        q_exec(
            &mut s,
            &format!(
                "insert[`{TBL};(2000.01.01D00:00:00+1000000000*75 90 150 210;\
                 `EARLY`AAPL`GOOG`MSFT;100 101 102 103f;10 20 30 40j)]"
            ),
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    let start = NanoTime::from_kdb_timestamp(90 * 1_000_000_000); // NOT 60s-aligned
    let g = GraphBuilder::new();
    let acc = kdb_read::<TestTradeWrite>(
        &g,
        historical(start, 180),
        conn.clone(),
        Duration::from_secs(60),
        move |(t0, t1), _date, _| {
            format!(
                "select from {TBL} where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                t0.to_kdb_timestamp(),
                t1.to_kdb_timestamp(),
            )
        },
        None,
    )?
    .collapse_accumulate();
    let mut runner = g.build();
    let run_result = runner.run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(180)),
    );

    let td = drop_table(&rt, &conn, TBL);
    run_result?; // must NOT abort despite the pre-start `EARLY` (75s) row
    td?;

    let syms: Vec<String> = runner
        .value(&acc)
        .iter()
        .map(|t| t.sym.to_string())
        .collect();
    assert_eq!(
        syms,
        vec!["AAPL", "GOOG", "MSFT"],
        "the pre-start EARLY row must be dropped"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Write tests
// ---------------------------------------------------------------------------

fn make_test_trades(n: usize) -> Vec<TestTrade> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    let mut interner = SymbolInterner::default();
    (0..n)
        .map(|i| TestTrade {
            sym: interner.intern(syms[i % syms.len()]),
            price: 100.0 + i as f64,
            qty: (i * 10 + 1) as i64,
        })
        .collect()
}

/// Write `trades` (one per second from the KDB epoch) into `WRITE_TABLE_NAME`.
fn write_trades(conn: KdbConnection, trades: Vec<TestTrade>) -> Result<()> {
    let g = GraphBuilder::new();
    let rows = trades
        .into_iter()
        .enumerate()
        .map(|(i, t)| Ok((t, NanoTime::from_kdb_timestamp(i as i64 * 1_000_000_000))));
    let _sink = g
        .replay_results(rows)
        .kdb_write(conn, WRITE_TABLE_NAME, None)?;
    let mut runner = g.build();
    // Match the classic write test: run Forever so the whole [0, MAX] window
    // covers the year-2000 `from_kdb_timestamp` data (a bounded Duration from
    // NanoTime::ZERO would exclude it); the finite replay closes the channel and
    // stops the graph.
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    Ok(())
}

#[test]
fn test_kdb_write_round_trip() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_write_round_trip") else {
        return Ok(());
    };
    rt.block_on(async {
        let mut s = connect(&conn).await?;
        q_exec(&mut s, &format!("{WRITE_TABLE_NAME}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())")).await?;
        Ok::<(), anyhow::Error>(())
    })?;

    let result: Result<()> = (|| {
        write_trades(conn.clone(), make_test_trades(5))?;
        assert_eq!(
            table_count(&rt, &conn, WRITE_TABLE_NAME)?,
            5,
            "should have written 5 trades"
        );

        // Read back and check the first row.
        let rows = read_rows::<TestTradeWrite>(
            conn.clone(),
            Duration::from_secs(24 * 3600),
            86400,
            |(t0, t1), _, _| {
                format!(
                    "select from {} where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                    WRITE_TABLE_NAME,
                    t0.to_kdb_timestamp(),
                    t1.to_kdb_timestamp(),
                )
            },
        )?;
        assert_eq!(rows.len(), 5, "should read back 5 rows");
        assert_eq!(rows[0].sym.to_string(), "AAPL");
        assert!((rows[0].price - 100.0).abs() < 0.001);
        assert_eq!(rows[0].qty, 1);
        Ok(())
    })();
    let td = drop_table(&rt, &conn, WRITE_TABLE_NAME);
    result?;
    td
}

#[test]
fn test_kdb_write_append() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_write_append") else {
        return Ok(());
    };
    // Pre-populate 3 rows, then append 2 more via the graph.
    rt.block_on(async {
        let mut s = connect(&conn).await?;
        q_exec(&mut s, &format!("{WRITE_TABLE_NAME}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())")).await?;
        q_exec(
            &mut s,
            &format!("insert[`{WRITE_TABLE_NAME};(2000.01.01D00:00:00.000000000+1000000000j*til 3;3?`AAPL`GOOG`MSFT;3?100.0;3?1000j)]"),
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    let result: Result<()> = (|| {
        // Append at timestamps after the existing rows (10s, 11s).
        let g = GraphBuilder::new();
        let rows = make_test_trades(2).into_iter().enumerate().map(|(i, t)| {
            Ok((
                t,
                NanoTime::from_kdb_timestamp((10 + i as i64) * 1_000_000_000),
            ))
        });
        let _sink = g
            .replay_results(rows)
            .kdb_write(conn.clone(), WRITE_TABLE_NAME, None)?;
        let mut runner = g.build();
        // Forever so the [0, MAX] window covers the year-2000 append timestamps
        // (see write_trades); the finite replay closes the channel and stops.
        runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        assert_eq!(
            table_count(&rt, &conn, WRITE_TABLE_NAME)?,
            5,
            "3 original + 2 appended = 5"
        );
        Ok(())
    })();
    let td = drop_table(&rt, &conn, WRITE_TABLE_NAME);
    result?;
    td
}

// ---------------------------------------------------------------------------
// Real-time subscription test
// ---------------------------------------------------------------------------

const SUB_TABLE_NAME: &str = "sub_trades";

#[derive(Debug, Clone, Default)]
struct TestTick {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTick {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((
            time,
            TestTick {
                sym: row.get_sym(1, interner)?,
                price: row.get(2)?.get_float()?,
                qty: row.get(3)?.get_long()?,
            },
        ))
    }
}

/// A minimal in-process tickerplant over IPC so a plain `q -p 5000` can act as
/// one (`.u.sub` / `.u.pub` / `.u.nsub`), mirroring the classic test.
const TICKERPLANT_INIT: &str = "\
sub_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$());\
.u.w:()!();\
.u.sub:{[t;s].u.w[t]:$[t in key .u.w;.u.w[t];()],enlist(.z.w;s);(t;0#value t)};\
.u.pub:{[t;x]{[t;x;ws]neg[ws 0](`upd;t;x)}[t;x]each .u.w[t]};\
.u.nsub:{$[x in key .u.w;count .u.w[x];0]}";

#[test]
fn test_kdb_sub_realtime_receives_published_rows() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_sub_realtime_receives_published_rows") else {
        return Ok(());
    };

    // 1. Set up the in-process tickerplant.
    rt.block_on(async {
        let mut s = connect(&conn).await?;
        q_exec(&mut s, TICKERPLANT_INIT).await?;
        Ok::<(), anyhow::Error>(())
    })?;

    // 2. Publisher thread: wait for the subscription, then publish 5 rows.
    let pub_conn = conn.clone();
    let publisher = std::thread::spawn(move || -> Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut s = connect(&pub_conn).await?;
            let mut registered = false;
            for _ in 0..250 {
                let n = q_exec(&mut s, &format!(".u.nsub[`{SUB_TABLE_NAME}]")).await?;
                if n.get_long().unwrap_or(0) > 0 {
                    registered = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if !registered {
                anyhow::bail!("kdb_sub never registered as a subscriber");
            }
            q_exec(&mut s, &format!(".u.pub[`{SUB_TABLE_NAME};(`timestamp$3#.z.p;`AAPL`GOOG`MSFT;100 101 102f;10 20 30j)]")).await?;
            q_exec(&mut s, &format!(".u.pub[`{SUB_TABLE_NAME};(enlist .z.p;enlist `AAPL;enlist 103f;enlist 40j)]")).await?;
            q_exec(&mut s, &format!(".u.pub[`{SUB_TABLE_NAME};(enlist .z.p;enlist `GOOG;enlist 104f;enlist 50j)]")).await?;
            Ok::<(), anyhow::Error>(())
        })
    });

    // 3. Subscribe on-graph in real time; accumulate the raw bursts (not
    //    `.collapse()`, which would drop rows from a multi-row upd).
    let g = GraphBuilder::new();
    let acc =
        kdb_sub::<TestTick>(&g, RunMode::RealTime, conn.clone(), SUB_TABLE_NAME, "`")?.accumulate();
    let mut runner = g.build();
    let run_result = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(5)));

    let publish_result = publisher.join().expect("publisher thread panicked");
    let teardown_result = rt.block_on(async {
        let mut s = connect(&conn).await?;
        q_exec(
            &mut s,
            &format!("delete {SUB_TABLE_NAME} from `.;.u.w:()!()"),
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    });

    run_result?;
    publish_result?;
    teardown_result?;

    // 4. Flatten per-cycle bursts; order + count are preserved.
    let bursts: Vec<Burst<TestTick>> = runner.value(&acc);
    let got: Vec<(String, f64, i64)> = bursts
        .iter()
        .flat_map(|b| b.iter())
        .map(|t| (t.sym.to_string(), t.price, t.qty))
        .collect();
    let expected = vec![
        ("AAPL".to_string(), 100.0, 10),
        ("GOOG".to_string(), 101.0, 20),
        ("MSFT".to_string(), 102.0, 30),
        ("AAPL".to_string(), 103.0, 40),
        ("GOOG".to_string(), 104.0, 50),
    ];
    assert_eq!(
        got, expected,
        "on-graph rows should match the published rows in order"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Cached read tests (port of cache_integration_tests.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct TestTradeCached {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTradeCached {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(1)?; // (date, time, sym, price, qty)
        Ok((
            time,
            TestTradeCached {
                sym: row.get_sym(2, interner)?,
                price: row.get(3)?.get_float()?,
                qty: row.get(4)?.get_long()?,
            },
        ))
    }
}

fn read_cached_count(conn: KdbConnection, cache_dir: &std::path::Path) -> Result<usize> {
    let start = NanoTime::from_kdb_timestamp(0);
    let g = GraphBuilder::new();
    let acc = kdb_read_cached::<TestTradeCached>(
        &g,
        historical(start, 86400),
        conn,
        Duration::from_secs(24 * 3600),
        CacheConfig::new(cache_dir, u64::MAX),
        |w, date, _| slice_query(date, w.0, w.1),
    )?
    .collapse_accumulate();
    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(86400)),
    )?;
    Ok(runner.value(&acc).len())
}

#[test]
fn test_kdb_read_cached_populates_and_hits() -> Result<()> {
    let Some((conn, rt)) = kdb_or_skip("test_kdb_read_cached_populates_and_hits") else {
        return Ok(());
    };
    setup_read_table(&rt, &conn, 3, 1)?;
    let cache_dir =
        std::env::temp_dir().join(format!("wingfoil-next-kdb-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache_dir);

    let result: Result<()> = (|| {
        // First run: cache miss — populates.
        assert_eq!(
            read_cached_count(conn.clone(), &cache_dir)?,
            3,
            "first run reads 3 rows"
        );
        let cache_files = std::fs::read_dir(&cache_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "cache"))
            .count();
        assert!(cache_files >= 1, "a cache file should have been written");
        // Second run: cache hit — same rows, no connection needed.
        assert_eq!(
            read_cached_count(conn.clone(), &cache_dir)?,
            3,
            "second run reads 3 rows from cache"
        );
        Ok(())
    })();
    let td = drop_table(&rt, &conn, TABLE_NAME);
    let _ = std::fs::remove_dir_all(&cache_dir);
    result?;
    td
}
