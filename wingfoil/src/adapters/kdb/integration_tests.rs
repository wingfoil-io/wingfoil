//! Integration tests for KDB+ read/write functionality.
//! These tests require a running kdb instance:
//! ```sh
//! q -p 5000
//! ```
//! The tests are disabled by default and require this feature flag to enable:
//! ```
//! RUST_LOG=INFO cargo test kdb::integration_tests  --features kdb-integration-test -p wingfoil -- --test-threads=1 --no-capture
//! ```

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
    ) -> Result<(NanoTime, Self), KdbError> {
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
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?; // col 0: time
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

/// Build a time-slice query for TABLE_NAME filtering by date and time range.
/// Half-open interval [t0, t1): `time >= t0, time < t1`.
fn slice_query(date: i32, t0: NanoTime, t1: NanoTime) -> String {
    format!(
        "select from {} where date=2000.01.01+{}, time >= (`timestamp$){}j, time < (`timestamp$){}j",
        TABLE_NAME,
        date,
        t0.to_kdb_timestamp(),
        t1.to_kdb_timestamp(),
    )
}

/*
bacon test -- --features kdb-integration-test -p wingfoil kdb::integration_tests -- --test-threads=1 --no-capture
*/

#[test]
fn test_kdb_sorted_data() -> Result<()> {
    let _ = env_logger::try_init();
    // 3 rows/day × 2 days = 6 rows total; one 24-hour slice per day.
    with_test_data(3, 2, true, |_n, conn| {
        let stream = kdb_read::<TestTrade, _>(
            conn,
            std::time::Duration::from_secs(24 * 3600),
            |within, date, _| slice_query(date, within.0, within.1),
        );
        let collected = stream.collapse().collect();
        collected.clone().run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(std::time::Duration::from_secs(2 * 86400)),
        )?;
        assert_eq!(
            collected.peek_value().len(),
            6,
            "Should read all 6 rows (3 per day × 2 days)"
        );
        Ok(())
    })
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
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(1)?; // col 0: date, col 1: time
        Ok((
            time,
            BadTrade {
                // col 2: sym is a symbol, but get_long() will fail — intentional for error testing
                sym: row.get(2)?.get_long()?,
            },
        ))
    }
}

#[test]
fn test_kdb_bad_query() -> Result<()> {
    let _ = env_logger::try_init();
    let conn = TestDataBuilder::connection();
    let stream = kdb_read::<TestTrade, _>(
        conn,
        std::time::Duration::from_secs(24 * 3600),
        |_, _, _| "select from nonexistent_table_xyz".to_string(),
    );
    let collected = stream.collapse().collect();
    let result = collected.run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    );
    assert!(result.is_err(), "Bad query should return an error");
    Ok(())
}

#[test]
fn test_kdb_deserialization_error() -> Result<()> {
    let _ = env_logger::try_init();
    let result = with_test_data(3, 1, true, |_n, conn| {
        let stream = kdb_read::<BadTrade, _>(
            conn,
            std::time::Duration::from_secs(24 * 3600),
            |within, date, _| slice_query(date, within.0, within.1),
        );
        let collected = stream.collapse().collect();
        collected.run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(std::time::Duration::from_secs(86400)),
        )?;
        Ok(())
    });
    assert!(
        result.is_err(),
        "Type mismatch should return a deserialization error"
    );
    Ok(())
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
        let periods = [
            std::time::Duration::from_secs(3600),      // 1h slices (24/day)
            std::time::Duration::from_secs(6 * 3600),  // 6h slices (4/day)
            std::time::Duration::from_secs(24 * 3600), // 1 slice/day
        ];

        println!("\n{:<15} {:>12}", "Period (secs)", "Time");
        println!("{}", "-".repeat(30));

        for &period in &periods {
            let start = std::time::Instant::now();
            let stream = kdb_read::<TestTrade, _>(conn.clone(), period, |within, date, _| {
                slice_query(date, within.0, within.1)
            });
            let counter = stream.collapse().count();
            counter.clone().run(
                RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
                RunFor::Duration(std::time::Duration::from_secs(num_days as u64 * 86400)),
            )?;
            assert_eq!(counter.peek_value() as usize, n);
            println!("{:<15} {:?}", period.as_secs(), start.elapsed());
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
    let stream = kdb_read::<TestTrade, _>(
        conn,
        std::time::Duration::from_secs(24 * 3600),
        |_, _, _| format!("select from {}", TABLE_NAME),
    );
    let collected = stream.collapse().collect();
    let _ = collected.run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    );
}

#[test]
fn test_kdb_empty_table_returns_zero_rows() -> Result<()> {
    let _ = env_logger::try_init();
    with_empty_table(|conn| {
        let stream = kdb_read::<TestTrade, _>(
            conn,
            std::time::Duration::from_secs(24 * 3600),
            |within, date, _| slice_query(date, within.0, within.1),
        );
        let collected = stream.collapse().collect();
        collected.clone().run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(std::time::Duration::from_secs(86400)),
        )?;
        assert_eq!(
            collected.peek_value().len(),
            0,
            "Empty table should return 0 rows"
        );
        Ok(())
    })
}

/// Test that `kdb_read` reads all rows across multiple time slices and days.
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
fn test_kdb_read_works() -> Result<()> {
    let _ = env_logger::try_init();

    with_test_data(3, 2, true, |_n, conn| {
        let start = NanoTime::from_kdb_timestamp(0); // 2000.01.01D00:00:00

        let stream = kdb_read::<TestTrade, _>(
            conn,
            std::time::Duration::from_secs(12 * 3600), // 12-hour slices
            move |(slice_start, slice_end), date, _iteration| {
                slice_query(date, slice_start, slice_end)
            },
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

        // Verify data correctness by reading back via kdb_read.
        // The write table has no date column so we filter by time only.
        let read_stream = kdb_read::<TestTradeWrite, _>(
            conn,
            std::time::Duration::from_secs(24 * 3600),
            move |(t0, t1), _, _| {
                format!(
                    "select from {} where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                    WRITE_TABLE_NAME,
                    t0.to_kdb_timestamp(),
                    t1.to_kdb_timestamp(),
                )
            },
        );
        let collected = read_stream
            .collapse()
            .logged("readback", Level::Info)
            .collect();
        collected.clone().run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(std::time::Duration::from_secs(86400)),
        )?;
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
