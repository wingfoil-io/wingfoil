//! Integration tests for KDB+ read/write functionality.
//! These tests require a running kdb instance:
//! ```sh
//! q -p 5000
//! ```
//! The tests are disabled by default and require this feature flag to enable:
//! ```
//! RUST_LOG=INFO cargo test kdb::integration_tests  --features kdb-integration-test -p wingfoil -- --test-threads=1 --no-capture
//! ```
//!
//!
//!

use super::*;
use crate::{RunFor, RunMode, nodes::*, types::*};
use anyhow::{Context, Result};
use derive_new::new;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use log::{Level, LevelFilter};
use tokio::runtime::Runtime;

/// Read tests table: (date, time, sym, price, qty)
const TABLE_NAME: &str = "test_trades";
/// Write tests table: (time, sym, price, qty) — no date, since kdb_write only prepends time
const WRITE_TABLE_NAME: &str = "test_trades_write";
/// Date-filter tests table: (date, time, sym, price, qty)
const DATED_TABLE_NAME: &str = "test_trades_dated";

#[derive(Debug, Clone, Default)]
pub struct TestTrade {
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
    ) -> Result<Self, KdbError> {
        Ok(TestTrade {
            // col 0: date (skip), col 1: time (handled by adapter)
            sym: row.get_sym(2, interner)?,
            price: row.get(3)?.get_float()?,
            qty: row.get(4)?.get_long()?,
        })
    }
}

/// TestTrade variant for the write table, which has no date column: (time, sym, price, qty).
#[derive(Debug, Clone, Default)]
pub struct TestTradeWrite {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTradeWrite {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<Self, KdbError> {
        Ok(TestTradeWrite {
            // col 0: time (handled by adapter)
            sym: row.get_sym(1, interner)?,
            price: row.get(2)?.get_float()?,
            qty: row.get(3)?.get_long()?,
        })
    }
}

/// Trade struct for tables with an explicit date column: (date, time, sym, price, qty).
///
/// Used to test date-partitioned table reads where the schema includes a `date` column
/// before the `time` column (e.g., splayed tables or in-memory tables with a date column).
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct TestTradeDate {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTradeDate {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<Self, KdbError> {
        Ok(TestTradeDate {
            // col 0: date (skip), col 1: time (adapter handles)
            sym: row.get_sym(2, interner)?,
            price: row.get(3)?.get_float()?,
            qty: row.get(4)?.get_long()?,
        })
    }
}

#[derive(new)]
struct TestDataBuilder {
    connection: KdbConnection,
    tokio: Runtime,
}

impl TestDataBuilder {
    fn connection() -> KdbConnection {
        let port = std::env::var("KDB_TEST_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(5000);
        let host = std::env::var("KDB_TEST_HOST").unwrap_or_else(|_| "localhost".to_string());
        KdbConnection::new(host, port)
    }

    async fn socket(&self) -> Result<QStream> {
        let creds = self.connection.credentials_string();
        QStream::connect(
            ConnectionMethod::TCP,
            &self.connection.host,
            self.connection.port,
            &creds,
        )
        .await
        .context("Failed to connect to KDB+")
    }

    async fn execute(&self, query: &str) -> Result<()> {
        //println!("{}", query);
        let result = self
            .socket()
            .await?
            .send_sync_message(&query)
            .await
            .context("Failed to send query to KDB+")?;

        if result.get_type() == -128 {
            anyhow::bail!("KDB+ query error: {:?}", result);
        }
        Ok(())
    }

    /// Create the read table with a date column: (date, time, sym, price, qty).
    async fn create_table(&self) -> Result<()> {
        self.execute(&format!(
            "{}:([]date:`date$();time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())",
            TABLE_NAME
        ))
        .await?;
        Ok(())
    }

    /// Insert rows into TABLE_NAME, parameterised by records per day and number of days.
    ///
    /// For sorted data, timestamps ascend within each day and days are in calendar order.
    /// For unsorted data, all rows land on 2000.01.01 with shuffled nanosecond offsets
    /// (sufficient to trigger the adapter's time-ordering check).
    async fn write_rows(
        &self,
        records_per_day: usize,
        num_days: usize,
        sorted: bool,
    ) -> Result<()> {
        let n = records_per_day * num_days;
        let (date_expr, time_expr) = if sorted {
            (
                // Each date repeated records_per_day times, num_days dates in order
                format!(
                    "raze {{{}#2000.01.01+x}} each til {}",
                    records_per_day, num_days
                ),
                // Within each day d: evenly distribute records across 24 hours so that
                // timestamps never spill into the next day regardless of records_per_day.
                // Interval = floor(86400s / records_per_day) in nanoseconds.
                format!(
                    "raze {{(`timestamp$2000.01.01+x)+(86400000000000j div {}j)*til {}}} each til {}",
                    records_per_day, records_per_day, num_days
                ),
            )
        } else {
            (
                // All rows on the same date; shuffled timestamps will go backwards
                format!("{}#2000.01.01", n),
                format!("2000.01.01D00:00:00.000000000+1000000000j*neg {}?{}", n, n),
            )
        };
        let query = format!(
            "insert[`{table};({dates};{times};{n}?`AAPL`GOOG`MSFT;{n}?100.0;{n}?1000j)]",
            table = TABLE_NAME,
            dates = date_expr,
            times = time_expr,
            n = n,
        );
        self.execute(&query).await?;
        Ok(())
    }

    async fn drop_table(&self) -> Result<()> {
        self.execute(&format!("delete {} from `.", TABLE_NAME))
            .await?;
        Ok(())
    }

    /// Create the write table without a date column: (time, sym, price, qty).
    async fn create_write_table(&self) -> Result<()> {
        self.execute(&format!(
            "{}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())",
            WRITE_TABLE_NAME
        ))
        .await?;
        Ok(())
    }

    /// Insert `n` sorted rows into WRITE_TABLE_NAME (for write-append test setup).
    async fn write_rows_to_write_table(&self, n: usize) -> Result<()> {
        let query = format!(
            "insert[`{table};(2000.01.01D00:00:00.000000000+1000000000j*til {n};{n}?`AAPL`GOOG`MSFT;{n}?100.0;{n}?1000j)]",
            table = WRITE_TABLE_NAME,
            n = n,
        );
        self.execute(&query).await?;
        Ok(())
    }

    async fn drop_write_table(&self) -> Result<()> {
        self.execute(&format!("delete {} from `.", WRITE_TABLE_NAME))
            .await?;
        Ok(())
    }

    /// Create a table with an explicit date column: (date, time, sym, price, qty).
    /// Used to test date partition filtering without requiring a true splayed DB on disk.
    async fn create_dated_table(&self) -> Result<()> {
        self.execute(&format!(
            "{}:([]date:`date$();time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())",
            DATED_TABLE_NAME
        ))
        .await
    }

    /// Insert `n` rows into the dated table on the given q date literal (e.g. `"2000.01.01"`).
    /// Timestamps are at midnight + 0s, 1s, 2s, ... on that date.
    async fn write_rows_on_date(&self, date_q: &str, n: usize) -> Result<()> {
        let query = format!(
            "insert[`{table};({n}#{date};{date}D00:00:00.000000000+1000000000j*til {n};{n}?`AAPL`GOOG`MSFT;{n}?100.0;{n}?1000j)]",
            table = DATED_TABLE_NAME,
            n = n,
            date = date_q,
        );
        self.execute(&query).await
    }

    async fn drop_dated_table(&self) -> Result<()> {
        self.execute(&format!("delete {} from `.", DATED_TABLE_NAME))
            .await
    }

    fn setup(&self, records_per_day: usize, num_days: usize, sorted: bool) -> Result<()> {
        self.tokio.block_on(async {
            self.create_table().await?;
            self.write_rows(records_per_day, num_days, sorted).await?;
            Ok(())
        })
    }

    fn teardown(&self) -> Result<()> {
        self.tokio.block_on(async { self.drop_table().await })
    }
}

fn with_test_data<F>(
    records_per_day: usize,
    num_days: usize,
    sorted: bool,
    test: F,
) -> anyhow::Result<()>
where
    F: FnOnce(usize, KdbConnection) -> anyhow::Result<()>,
{
    let conn = TestDataBuilder::connection();
    let rt = tokio::runtime::Runtime::new()?;
    let builder = TestDataBuilder::new(conn.clone(), rt);
    builder.setup(records_per_day, num_days, sorted)?;
    let test_result = test(records_per_day * num_days, conn);
    let teardown_result = builder.teardown();
    test_result?;
    teardown_result?;
    Ok(())
}

/// Helper: creates a dated table, populates it with `rows_per_day` rows on each of the
/// given `dates` (q date literals like `"2000.01.01"`), runs the test, then drops the table.
fn with_dated_test_data<F>(rows_per_day: usize, dates: &[&str], test: F) -> anyhow::Result<()>
where
    F: FnOnce(KdbConnection) -> anyhow::Result<()>,
{
    let conn = TestDataBuilder::connection();
    let rt = tokio::runtime::Runtime::new()?;
    let builder = TestDataBuilder::new(conn.clone(), rt);
    builder.tokio.block_on(async {
        builder.create_dated_table().await?;
        for &date in dates {
            builder.write_rows_on_date(date, rows_per_day).await?;
        }
        Ok::<_, anyhow::Error>(())
    })?;
    let test_result = test(conn);
    let teardown_result = builder.tokio.block_on(builder.drop_dated_table());
    test_result?;
    teardown_result?;
    Ok(())
}

/// Helper: creates an empty TABLE_NAME (with date column), runs the test, then drops it.
fn with_empty_table<F>(test: F) -> Result<()>
where
    F: FnOnce(KdbConnection) -> Result<()>,
{
    let conn = TestDataBuilder::connection();
    let rt = tokio::runtime::Runtime::new()?;
    let builder = TestDataBuilder::new(conn.clone(), rt);
    builder.tokio.block_on(builder.create_table())?;
    let test_result = test(conn);
    let teardown_result = builder.teardown();
    test_result?;
    teardown_result?;
    Ok(())
}

/// Helper: creates an empty WRITE_TABLE_NAME (no date column), runs the test, then drops it.
fn with_empty_write_table<F>(test: F) -> Result<()>
where
    F: FnOnce(KdbConnection) -> Result<()>,
{
    let conn = TestDataBuilder::connection();
    let rt = tokio::runtime::Runtime::new()?;
    let builder = TestDataBuilder::new(conn.clone(), rt);
    builder.tokio.block_on(builder.create_write_table())?;
    let test_result = test(conn);
    let teardown_result = builder.tokio.block_on(builder.drop_write_table());
    test_result?;
    teardown_result?;
    Ok(())
}

fn read(
    query: &str,
    time_col: &str,
    records_per_day: usize,
    num_days: usize,
    chunk_size: usize,
    sorted: bool,
) -> anyhow::Result<usize> {
    let mut count = 0;
    with_test_data(records_per_day, num_days, sorted, |n, conn| {
        let trades = kdb_read::<TestTrade>(conn, query, time_col, None::<&str>, chunk_size);
        let collected = trades.collapse().logged("trades", Level::Info).collect();
        collected
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        count = collected.peek_value().len();
        println!("Read {} rows (expected {})", count, n);
        Ok(())
    })?;
    Ok(count)
}

/*
bacon test -- --features kdb-integration-test -p wingfoil kdb::integration_tests -- --test-threads=1 --no-capture
*/

#[test]
fn test_kdb_sorted_data() -> Result<()> {
    let _ = env_logger::try_init();
    let count = read(
        &format!("select from {}", TABLE_NAME),
        "time",
        3, // records_per_day
        2, // num_days
        2, // chunk_size
        true,
    )?;
    assert_eq!(
        count, 6,
        "Should read all 6 rows (3 per day × 2 days) from sorted data"
    );
    Ok(())
}

#[test]
fn test_kdb_unsorted_data_fails() -> Result<()> {
    let _ = env_logger::try_init();
    // With unsorted data, the adapter detects time going backwards
    let result = read(
        &format!("select from {}", TABLE_NAME),
        "time",
        5, // records_per_day
        2, // num_days
        3, // chunk_size
        false,
    );
    assert!(
        result.is_err(),
        "Unsorted data should cause time ordering error"
    );
    let err = result.unwrap_err();
    let err_msg = format!("{:?}", err);
    // Unsorted data is caught either by the KDB adapter (within a chunk → "not sorted by time"
    // + "xasc" hint) or by the graph engine (between chunks → "time less than graph time").
    // Both are valid detections of the same underlying problem.
    assert!(
        err_msg.contains("not sorted by time") || err_msg.contains("time less than graph time"),
        "Expected time ordering error, got: {}",
        err_msg
    );
    println!("As expected, unsorted data caused error with helpful message");
    Ok(())
}

/// A struct that deliberately reads the wrong types to trigger deserialization errors.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct BadTrade {
    sym: i64, // sym is actually a symbol, not a long
}

impl KdbDeserialize for BadTrade {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        _interner: &mut SymbolInterner,
    ) -> Result<Self, KdbError> {
        Ok(BadTrade {
            // col 0: date (skip), col 1: time (handled by adapter), col 2: sym (symbol → get_long fails)
            sym: row.get(2)?.get_long()?,
        })
    }
}

fn read_with_type<T: Element + Send + KdbDeserialize>(
    query: &str,
    time_col: &str,
    records_per_day: usize,
    num_days: usize,
    chunk_size: usize,
) -> anyhow::Result<()> {
    with_test_data(records_per_day, num_days, true, |_n, conn| {
        let stream = kdb_read::<T>(conn, query, time_col, None::<&str>, chunk_size);
        let collected = stream.collapse().logged("trades", Level::Info).collect();
        collected.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        Ok(())
    })
}

#[test]
fn test_kdb_bad_query() -> Result<()> {
    let _ = env_logger::try_init();
    let result = read("select from nonexistent_table_xyz", "time", 3, 2, 2, true);
    assert!(result.is_err(), "Bad query should return an error");
    let err_msg = format!("{:?}", result.unwrap_err());
    println!("Bad query error: {}", err_msg);
    assert!(
        err_msg.contains("kdb query failed"),
        "Expected query error, got: {}",
        err_msg
    );
    Ok(())
}

#[test]
fn test_kdb_bad_time_column() -> Result<()> {
    let _ = env_logger::try_init();
    // The time column name is injected into the WHERE clause as a time filter, so KDB+
    // rejects the query with a -128 error when the column doesn't exist.
    let result = read(
        &format!("select from {}", TABLE_NAME),
        "nonexistent_col",
        3, // records_per_day
        2, // num_days
        2, // chunk_size
        true,
    );
    assert!(result.is_err(), "Bad time column should return an error");
    let err_msg = format!("{:?}", result.unwrap_err());
    println!("Bad time column error: {}", err_msg);
    assert!(
        err_msg.contains("kdb query failed"),
        "Expected kdb query failed error, got: {}",
        err_msg
    );
    Ok(())
}

#[test]
fn test_kdb_deserialization_error() -> Result<()> {
    let _ = env_logger::try_init();
    let result = read_with_type::<BadTrade>(
        &format!("select from {}", TABLE_NAME),
        "time",
        3, // records_per_day
        2, // num_days
        2, // chunk_size
    );
    assert!(
        result.is_err(),
        "Type mismatch should return a deserialization error"
    );
    let err_msg = format!("{:?}", result.unwrap_err());
    println!("Deserialization error: {}", err_msg);
    assert!(
        err_msg.contains("deserialization failed") || err_msg.contains("KDB"),
        "Expected deserialization error, got: {}",
        err_msg
    );
    Ok(())
}

fn read_rows(
    connection: KdbConnection,
    chunk_size: usize,
    log: bool,
) -> Result<(u64, std::time::Duration)> {
    let start = std::time::Instant::now();
    let mut trades = kdb_read::<TestTrade>(
        connection,
        &format!("select from {}", TABLE_NAME),
        "time",
        None::<&str>,
        chunk_size,
    );
    if log {
        trades = trades.logged(">>", Level::Info)
    };
    let trades = trades.count();
    trades.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    let count = trades.peek_value();
    Ok((count, start.elapsed()))
}

#[test]
fn kdb_read_works() -> Result<()> {
    let _ = env_logger::try_init();
    with_test_data(5, 2, true, |_n, conn| {
        let _ = read_rows(conn.clone(), 5, true)?;
        Ok(())
    })?;
    Ok(())
}

#[test]
fn kdb_read_splayed_works() -> Result<()> {
    let _ = env_logger::try_init();
    with_dated_test_data(3, &["2000.01.01", "2000.01.02"], |conn| {
        let trades = kdb_read::<TestTradeDate>(
            conn,
            &format!("select from {}", DATED_TABLE_NAME),
            "time",
            Some("date"),
            2, // chunk smaller than total rows to exercise multi-chunk reads
        );
        let trades = trades.logged(">>", Level::Info).count();
        trades.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        let count = trades.peek_value();
        assert_eq!(count, 6, "Should read all 6 rows (3 per day × 2 days)");
        Ok(())
    })
}

#[test]
fn kdb_read_splayed_from_to_multi_day() -> Result<()> {
    let _ = env_logger::try_init();
    // 9 rows: 3 per day on 2000.01.01, 2000.01.02, 2000.01.03 at t=0s, 1s, 2s.
    // start = KDB epoch + 0.5s  → excludes row 1 (2000.01.01D00:00:00), includes rows 2–9
    // end   = start + 2 days    → 2000.01.03D00:00:00.5, includes row 7, excludes rows 8–9
    // Expected: rows 2–7 = 6 rows (truncated 1 at start, 2 at end)
    with_dated_test_data(3, &["2000.01.01", "2000.01.02", "2000.01.03"], |conn| {
        let trades = kdb_read::<TestTradeDate>(
            conn,
            &format!("select from {}", DATED_TABLE_NAME),
            "time",
            Some("date"),
            10,
        );
        let trades = trades.logged(">>", Level::Info).count();
        let start = NanoTime::from_kdb_timestamp(500_000_000); // 2000.01.01D00:00:00.5
        trades.run(
            RunMode::HistoricalFrom(start),
            RunFor::Duration(std::time::Duration::from_secs(172_800)), // 2 days
        )?;
        let count = trades.peek_value();
        assert_eq!(
            count, 6,
            "Should read 6 rows: first and last 2 truncated by start/end filter"
        );
        Ok(())
    })
}

#[test]
fn kdb_read_splayed_from_to() -> Result<()> {
    let _ = env_logger::try_init();
    // 6 rows total: day 1 at 00:00:00, 00:00:01, 00:00:02 and day 2 the same.
    // start = 2000.01.01D00:00:01 → excludes row 1 (truncates 1 from start)
    // end   = start + 12h        → 2000.01.01D12:00:01, excludes all 3 day-2 rows (truncates 3 from end)
    // Expected: rows 2–3 = 2 rows
    with_dated_test_data(3, &["2000.01.01", "2000.01.02"], |conn| {
        let trades = kdb_read::<TestTradeDate>(
            conn,
            &format!("select from {}", DATED_TABLE_NAME),
            "time",
            Some("date"),
            10,
        );
        let trades = trades.logged(">>", Level::Info).count();
        let start = NanoTime::from_kdb_timestamp(1_000_000_000); // 2000.01.01D00:00:01
        trades.run(
            RunMode::HistoricalFrom(start),
            RunFor::Duration(std::time::Duration::from_secs(43200)), // 12 hours
        )?;
        let count = trades.peek_value();
        assert_eq!(
            count, 2,
            "Row 1 truncated by start, all 3 day-2 rows truncated by end"
        );
        Ok(())
    })
}

#[test]
fn test_read_read_perf() -> Result<()> {
    /*
    cargo flamegraph --open --unit-test -p wingfoil --features kdb-integration-test -- kdb::integration_tests::test_read_read_perf --nocapture
    cargo test --release  -p wingfoil --features kdb-integration-test -- kdb::integration_tests::test_read_read_perf --nocapture
     */
    log::set_max_level(LevelFilter::Off);
    let records_per_day = 100_000;
    let num_days = 10;

    with_test_data(records_per_day, num_days, true, |n, conn| {
        let chunk_sizes = [
            //100,
            1_000, 10_000, 100_000, 1_000_000, 10_000_000,
        ];

        println!("\n{:<15} {:>12}", "Chunk Size", "Time");
        println!("{}", "-".repeat(45));

        for &chunk_size in &chunk_sizes {
            let (count, elapsed) = read_rows(conn.clone(), chunk_size, false)?;
            assert_eq!(count as usize, n);
            println!("{:<15} {:?}", chunk_size, elapsed);
        }

        Ok(())
    })
}

#[test]
#[should_panic(expected = "Closed")]
fn test_kdb_connection_refused() {
    let _ = env_logger::try_init();
    // Connection failure currently panics in the channel layer (kanal_chan recv().unwrap())
    // rather than propagating as a clean Err. This test documents that behavior.
    let conn = KdbConnection::new("localhost", 59999);
    let stream = kdb_read::<TestTrade>(
        conn,
        &format!("select from {}", TABLE_NAME),
        "time",
        None::<&str>,
        100,
    );
    let collected = stream.collapse().collect();
    let _ = collected.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
}

#[test]
fn test_kdb_empty_table_returns_zero_rows() -> Result<()> {
    let _ = env_logger::try_init();
    with_empty_table(|conn| {
        let trades = kdb_read::<TestTrade>(
            conn,
            &format!("select from {}", TABLE_NAME),
            "time",
            None::<&str>,
            1000,
        );
        let collected = trades.collapse().collect();
        collected
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        let rows = collected.peek_value();
        assert_eq!(rows.len(), 0, "Empty table should return 0 rows");
        Ok(())
    })
}

/// Test that `date_col=Some("date")` restricts results to the specified date partition,
/// and `date_col=None` returns all rows regardless of date.
///
/// Uses an in-memory table with an explicit `date` column to simulate partitioned table
/// behaviour without requiring a splayed database on disk.
#[test]
fn test_kdb_date_filter() -> Result<()> {
    let _ = env_logger::try_init();

    // 3 rows on 2000-01-01 (kdb_date=0) and 3 rows on 2000-01-02 (kdb_date=1)
    with_dated_test_data(3, &["2000.01.01", "2000.01.02"], |conn| {
        // date_col=Some("date"), half-day duration starting at KDB epoch (2000-01-01):
        //   start_kdb_date = 0, end_kdb_date = 0 → `date within (2000.01.01;2000.01.01)`
        //   → only day-0 rows returned → 3 rows
        let stream = kdb_read::<TestTradeDate>(
            conn.clone(),
            &format!("select from {}", DATED_TABLE_NAME),
            "time",
            Some("date"),
            1000,
        );
        let collected = stream.collapse().collect();
        collected.clone().run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(std::time::Duration::from_secs(43200)), // 0.5 day → end_date still day 0
        )?;
        let rows = collected.peek_value();
        assert_eq!(
            rows.len(),
            3,
            "date filter (2000.01.01;2000.01.01) should return 3 rows, got {}",
            rows.len()
        );

        // date_col=None: no date filter → all 6 rows returned
        let stream2 = kdb_read::<TestTradeDate>(
            conn,
            &format!("select from {}", DATED_TABLE_NAME),
            "time",
            None::<&str>,
            1000,
        );
        let collected2 = stream2.collapse().collect();
        collected2
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        let rows2 = collected2.peek_value();
        assert_eq!(
            rows2.len(),
            6,
            "no date filter should return all 6 rows, got {}",
            rows2.len()
        );

        Ok(())
    })
}

/// Test that `kdb_read_chunks` correctly advances through date partitions.
///
/// Uses a chunk size larger than the rows-per-date so each query returns a partial
/// chunk, signalling to the closure that the date is exhausted and it should advance.
#[test]
fn test_kdb_read_chunks_date_advance() -> Result<()> {
    let _ = env_logger::try_init();

    // 3 rows on each of 2 dates; chunk=10 ensures each query is partial (3 < 10)
    with_dated_test_data(3, &["2000.01.01", "2000.01.02"], |conn| {
        let dates = ["2000.01.01", "2000.01.02"];
        let chunk = 10usize;
        let mut di = 0usize;
        let mut offset = 0usize;

        let stream = kdb_read_chunks::<TestTradeDate, _>(
            conn,
            move |last_count| {
                match last_count {
                    None => {} // first call: use date[0], offset 0
                    Some(n) if n < chunk => {
                        // partial chunk: date exhausted
                        di += 1;
                        offset = 0;
                    }
                    Some(n) => offset += n, // full chunk: more rows on same date
                }
                if di >= dates.len() {
                    return None;
                }
                Some(format!(
                    "select[{},{}] from {} where date={}",
                    offset, chunk, DATED_TABLE_NAME, dates[di]
                ))
            },
            "time",
        );

        let collected = stream.collapse().collect();
        collected
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        let rows = collected.peek_value();
        assert_eq!(
            rows.len(),
            6,
            "should read 3 rows × 2 dates = 6 rows, got {}",
            rows.len()
        );
        Ok(())
    })
}

/// Test that `kdb_read_time_sliced` reads all rows across multiple time slices and days.
///
/// Setup: 3 rows × 2 days (6 rows total).  Rows are evenly distributed across 24 h:
/// offsets at 0 h, 8 h, 16 h.
///
/// Period = 12 h → 2 slices per day (4 slices total):
///   Day 0, slice 0 [00:00, 12:00) → rows at 0 h and 8 h → 2 rows
///   Day 0, slice 1 [12:00, 23:59…] → row  at 16 h        → 1 row
///   Day 1, slice 0 [00:00, 12:00) → rows at 0 h and 8 h → 2 rows
///   Day 1, slice 1 [12:00, 23:59…] → row  at 16 h        → 1 row
/// Expected total: 6 rows.
#[test]
fn test_kdb_read_time_sliced_basic() -> Result<()> {
    let _ = env_logger::try_init();

    with_test_data(3, 2, true, |_n, conn| {
        let start = NanoTime::from_kdb_timestamp(0); // 2000.01.01D00:00:00

        let stream = kdb_read_time_sliced::<TestTrade, _>(
            conn,
            std::time::Duration::from_secs(12 * 3600), // 12-hour slices
            move |(slice_start, slice_end), date, _iteration| {
                // Build a q query filtered to this date and time range.
                // KDB `timestamp` type stores nanoseconds from 2000-01-01, so we use
                // `(`timestamp$)Nj` to cast the raw integer directly.
                Some(format!(
                    "select from {} where date=2000.01.01+{}, time within ((`timestamp$){}j;(`timestamp$){}j)",
                    TABLE_NAME,
                    date,
                    slice_start.to_kdb_timestamp(),
                    slice_end.to_kdb_timestamp(),
                ))
            },
            "time",
        );

        let collected = stream.collapse().collect();
        collected.clone().run(
            RunMode::HistoricalFrom(start),
            RunFor::Duration(std::time::Duration::from_secs(2 * 86400)),
        )?;
        let rows = collected.peek_value();
        assert_eq!(
            rows.len(),
            6,
            "Should read all 6 rows (3 per day × 2 days) across 4 time slices, got {}",
            rows.len()
        );
        Ok(())
    })
}

/// Test that `kdb_read_time_sliced` stops cleanly on empty slices.
///
/// Uses a period larger than a full day so each day produces exactly one slice.
/// The query_fn returns `None` after the first day to verify early termination.
#[test]
fn test_kdb_read_time_sliced_none_stops_stream() -> Result<()> {
    let _ = env_logger::try_init();

    with_test_data(3, 2, true, |_n, conn| {
        let start = NanoTime::from_kdb_timestamp(0);
        let mut slice_count = 0usize;

        let stream = kdb_read_time_sliced::<TestTrade, _>(
            conn,
            std::time::Duration::from_secs(24 * 3600), // one slice per day
            move |(slice_start, slice_end), date, _iteration| {
                if slice_count >= 1 {
                    // Stop after the first slice (day 0)
                    return None;
                }
                slice_count += 1;
                Some(format!(
                    "select from {} where date=2000.01.01+{}, time within ((`timestamp$){}j;(`timestamp$){}j)",
                    TABLE_NAME,
                    date,
                    slice_start.to_kdb_timestamp(),
                    slice_end.to_kdb_timestamp(),
                ))
            },
            "time",
        );

        let collected = stream.collapse().collect();
        collected.clone().run(
            RunMode::HistoricalFrom(start),
            RunFor::Duration(std::time::Duration::from_secs(2 * 86400)),
        )?;
        let rows = collected.peek_value();
        assert_eq!(
            rows.len(),
            3,
            "Should read only 3 rows (day 0) when query_fn returns None after first slice, got {}",
            rows.len()
        );
        Ok(())
    })
}

// --- Write integration tests ---

/// Helper: creates an empty WRITE_TABLE_NAME, writes trades via the graph, queries KDB to verify.
fn write_and_verify(conn: KdbConnection, trades: Vec<TestTrade>) -> Result<usize> {
    let n = trades.len();

    // Build a produce_async stream that yields each trade at a distinct timestamp
    let write_conn = conn.clone();
    let stream = produce_async(move |_ctx| {
        let trades = trades;
        async move {
            Ok(async_stream::stream! {
                for (i, trade) in trades.into_iter().enumerate() {
                    // Use KDB epoch + i seconds as timestamp
                    let time = NanoTime::from_kdb_timestamp(i as i64 * 1_000_000_000);
                    yield Ok((time, trade));
                }
            })
        }
    });

    // Write to KDB
    let writer = kdb_write(write_conn, WRITE_TABLE_NAME, &stream);
    writer.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    // Read back via raw KDB query to verify
    let rt = tokio::runtime::Runtime::new()?;
    let verify_conn = conn;
    let count = rt.block_on(async {
        let creds = verify_conn.credentials_string();
        let mut socket = QStream::connect(
            ConnectionMethod::TCP,
            &verify_conn.host,
            verify_conn.port,
            &creds,
        )
        .await?;
        let query = format!("count {}", WRITE_TABLE_NAME);
        let result = socket.send_sync_message(&query.as_str()).await?;
        let count = result.get_long()?;
        Ok::<i64, anyhow::Error>(count)
    })?;

    println!("Wrote {} trades, verified {} in KDB", n, count);
    Ok(count as usize)
}

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

#[test]
fn test_kdb_write_round_trip() -> Result<()> {
    let _ = env_logger::try_init();
    let trades = make_test_trades(5);

    with_empty_write_table(|conn| {
        let count = write_and_verify(conn.clone(), trades)?;
        assert_eq!(count, 5, "Should have written 5 trades");

        // Verify data correctness by reading back via kdb_read
        let read_stream = kdb_read::<TestTradeWrite>(
            conn,
            &format!("select from {}", WRITE_TABLE_NAME),
            "time",
            None::<&str>,
            10000,
        );
        let collected = read_stream
            .collapse()
            .logged("readback", Level::Info)
            .collect();
        collected
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        let rows = collected.peek_value();
        assert_eq!(rows.len(), 5, "Should read back 5 rows");

        // Check first row values (collect returns ValueAt<T>, access .value for the trade)
        let first = &rows[0].value;
        assert_eq!(first.sym.to_string(), "AAPL");
        assert!((first.price - 100.0).abs() < 0.001);
        assert_eq!(first.qty, 1);

        Ok(())
    })
}

#[test]
fn test_kdb_write_append() -> Result<()> {
    let _ = env_logger::try_init();

    let conn = TestDataBuilder::connection();
    let rt = tokio::runtime::Runtime::new()?;
    let builder = TestDataBuilder::new(conn.clone(), rt);

    // Create write table and pre-populate with 3 rows
    builder.tokio.block_on(async {
        builder.create_write_table().await?;
        builder.write_rows_to_write_table(3).await
    })?;

    let test_result: anyhow::Result<()> = (|| {
        let new_trades = make_test_trades(2);

        // Write 2 more trades via the graph - use timestamps after existing data
        let write_conn = conn.clone();
        let stream = produce_async(move |_ctx| {
            let trades = new_trades;
            async move {
                Ok(async_stream::stream! {
                    for (i, trade) in trades.into_iter().enumerate() {
                        // Use timestamps after the existing 3 rows (which use 0..3 seconds)
                        let time = NanoTime::from_kdb_timestamp((10 + i as i64) * 1_000_000_000);
                        yield Ok((time, trade));
                    }
                })
            }
        });

        let writer = kdb_write(write_conn, WRITE_TABLE_NAME, &stream);
        writer.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

        // Verify total count = 3 original + 2 new = 5
        let rt = tokio::runtime::Runtime::new()?;
        let count = rt.block_on(async {
            let creds = conn.credentials_string();
            let mut socket =
                QStream::connect(ConnectionMethod::TCP, &conn.host, conn.port, &creds).await?;
            let query = format!("count {}", WRITE_TABLE_NAME);
            let result = socket.send_sync_message(&query.as_str()).await?;
            result.get_long().map_err(anyhow::Error::new)
        })?;

        assert_eq!(count, 5, "Should have 3 original + 2 appended = 5 rows");
        println!("Append test: 3 + 2 = {} rows", count);
        Ok(())
    })();

    let teardown_result = builder.tokio.block_on(builder.drop_write_table());
    test_result?;
    teardown_result?;
    Ok(())
}
