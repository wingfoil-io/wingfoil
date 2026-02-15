//! Integration tests for KDB+ read/write functionality.
//! These tests require a running kdb instance:
//! ```sh
//! q -p 5000
//! ```
//! The tests are disabled by default and require this feature flag to enable:
//! ```
//! cargo test kdb::integration_tests  --features kdb-integration-test -p wingfoil -- --test-threads=1 --no-capture
//! ```
//!
//!
//!

use super::*;
use crate::{RunFor, RunMode, nodes::*, types::*};
use anyhow::{Context, Result};
use derive_new::new;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use log::Level;
use tokio::runtime::Runtime;

const TABLE_NAME: &str = "test_trades";

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
            // Skip column 0 (time) - it's handled by the adapter
            sym: row.get_sym(1, interner)?,
            price: row.get(2)?.get_float()?,
            qty: row.get(3)?.get_long()?,
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

    async fn create_table(&self) -> Result<()> {
        self.execute(&format!(
            "{}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())",
            TABLE_NAME
        ))
        .await?;
        Ok(())
    }

    async fn write_rows(&self, n: usize, sorted: bool) -> Result<()> {
        // Generate timestamps - sorted uses til, unsorted uses neg n?n to shuffle
        let time_expr = if sorted {
            format!("til {}", n)
        } else {
            format!("neg {}?{}", n, n)
        };
        let query = format!(
            "insert[`{};(2000.01.01D00:00:00.000000000+1000000000*{};{}?`AAPL`GOOG`MSFT;{}?100.0;{}?1000j)]",
            TABLE_NAME, time_expr, n, n, n
        );
        self.execute(&query).await?;
        Ok(())
    }

    async fn drop_table(&self) -> Result<()> {
        self.execute(&format!("delete {} from `.", TABLE_NAME))
            .await?;
        Ok(())
    }

    fn setup(&self, n: usize, sorted: bool) -> Result<()> {
        self.tokio.block_on(async {
            self.create_table().await?;
            self.write_rows(n, sorted).await?;
            Ok(())
        })
    }

    fn teardown(&self) -> Result<()> {
        self.tokio.block_on(async { self.drop_table().await })
    }
}

fn with_test_data<F>(n: usize, sorted: bool, test: F) -> anyhow::Result<()>
where
    F: FnOnce(usize, KdbConnection) -> anyhow::Result<()>,
{
    let conn = TestDataBuilder::connection();
    let rt = tokio::runtime::Runtime::new()?;
    let builder = TestDataBuilder::new(conn.clone(), rt);
    builder.setup(n, sorted)?;
    let test_result = test(n, conn);
    let teardown_result = builder.teardown();
    test_result?;
    teardown_result?;
    Ok(())
}

fn read(
    query: &str,
    time_col: &str,
    n: usize,
    chunk_size: usize,
    sorted: bool,
) -> anyhow::Result<usize> {
    let mut count = 0;
    with_test_data(n, sorted, |_n, conn| {
        let trades = kdb_read::<TestTrade>(conn, query, time_col, chunk_size);
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
    let count = read(&format!("select from {}", TABLE_NAME), "time", 6, 2, true)?;
    assert_eq!(count, 6, "Should read all 6 rows from sorted data");
    Ok(())
}

#[test]
fn test_kdb_unsorted_data_fails() -> Result<()> {
    let _ = env_logger::try_init();
    // With unsorted data, the adapter detects time going backwards
    let result = read(&format!("select from {}", TABLE_NAME), "time", 10, 3, false);
    assert!(
        result.is_err(),
        "Unsorted data should cause time ordering error"
    );
    let err = result.unwrap_err();
    let err_msg = format!("{:?}", err);
    assert!(
        err_msg.contains("not sorted by time"),
        "Expected unsorted data error, got: {}",
        err_msg
    );
    assert!(
        err_msg.contains("xasc"),
        "Error should suggest using xasc to sort, got: {}",
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
            sym: row.get(1)?.get_long()?, // sym column is symbol, get_long will fail
        })
    }
}

fn read_with_type<T: Element + Send + KdbDeserialize>(
    query: &str,
    time_col: &str,
    n: usize,
    chunk_size: usize,
) -> anyhow::Result<()> {
    with_test_data(n, true, |_n, conn| {
        let stream = kdb_read::<T>(conn, query, time_col, chunk_size);
        let collected = stream.collapse().logged("trades", Level::Info).collect();
        collected.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
        Ok(())
    })
}

#[test]
fn test_kdb_bad_query() -> Result<()> {
    let _ = env_logger::try_init();
    let result = read("select from nonexistent_table_xyz", "time", 6, 2, true);
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
    // The bad column name gets injected into the chunk query's WHERE clause,
    // causing KDB to reject the entire query (not a "column not found" from our code).
    let result = read(
        &format!("select from {}", TABLE_NAME),
        "nonexistent_col",
        6,
        2,
        true,
    );
    assert!(result.is_err(), "Bad time column should return an error");
    let err_msg = format!("{:?}", result.unwrap_err());
    println!("Bad time column error: {}", err_msg);
    assert!(
        err_msg.contains("kdb query failed"),
        "Expected query failure from bad column, got: {}",
        err_msg
    );
    Ok(())
}

#[test]
fn test_kdb_deserialization_error() -> Result<()> {
    let _ = env_logger::try_init();
    let result = read_with_type::<BadTrade>(&format!("select from {}", TABLE_NAME), "time", 6, 2);
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

fn read_rows(connection: KdbConnection, chunk_size: usize) -> Result<(u64, std::time::Duration)> {
    let start = std::time::Instant::now();
    let trades = kdb_read::<TestTrade>(
        connection,
        &format!("select from {}", TABLE_NAME),
        "time",
        chunk_size,
    )
    .count();
    trades.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    let count = trades.peek_value();
    Ok((count, start.elapsed()))
}

#[test]
fn test_read_read_perf() -> Result<()> {
    /*
    cargo flamegraph --open --unit-test -p wingfoil --features kdb-integration-test -- kdb::integration_tests::test_read_read_perf --nocapture
    cargo test --release  -p wingfoil --features kdb-integration-test -- kdb::integration_tests::test_read_read_perf --nocapture
     */
    let _ = env_logger::try_init();
    let n = 1_000_000;

    with_test_data(n, true, |n, conn| {
        let chunk_sizes = [
            //100,
            1_000, 10_000, 100_000, 1_000_000, 10_000_000,
        ];

        println!("\n{:<15} {:>12}", "Chunk Size", "Time");
        println!("{}", "-".repeat(45));

        for &chunk_size in &chunk_sizes {
            let (count, elapsed) = read_rows(conn.clone(), chunk_size)?;
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
    let stream = kdb_read::<TestTrade>(conn, &format!("select from {}", TABLE_NAME), "time", 100);
    let collected = stream.collapse().collect();
    let _ = collected.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
}

// --- Write integration tests ---

/// Helper: creates an empty table, writes trades via the graph, queries KDB to verify.
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
    let writer = kdb_write(write_conn, TABLE_NAME, &stream);
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
        let query = format!("count {}", TABLE_NAME);
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

/// Helper: creates empty table, runs test, drops table.
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

#[test]
fn test_kdb_write_round_trip() -> Result<()> {
    let _ = env_logger::try_init();
    let trades = make_test_trades(5);

    with_empty_table(|conn| {
        let count = write_and_verify(conn.clone(), trades)?;
        assert_eq!(count, 5, "Should have written 5 trades");

        // Verify data correctness by reading back via kdb_read
        let read_stream =
            kdb_read::<TestTrade>(conn, &format!("select from {}", TABLE_NAME), "time", 10000);
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

    with_test_data(3, true, |_n, conn| {
        // Table already has 3 rows from with_test_data
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

        let writer = kdb_write(write_conn, TABLE_NAME, &stream);
        writer.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

        // Verify total count = 3 original + 2 new = 5
        let rt = tokio::runtime::Runtime::new()?;
        let count = rt.block_on(async {
            let creds = conn.credentials_string();
            let mut socket =
                QStream::connect(ConnectionMethod::TCP, &conn.host, conn.port, &creds).await?;
            let query = format!("count {}", TABLE_NAME);
            let result = socket.send_sync_message(&query.as_str()).await?;
            result.get_long().map_err(|e| anyhow::Error::new(e))
        })?;

        assert_eq!(count, 5, "Should have 3 original + 2 appended = 5 rows");
        println!("Append test: 3 + 2 = {} rows", count);
        Ok(())
    })
}
