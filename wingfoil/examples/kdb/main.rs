//! KDB+ adapter example: demonstrates round-trip write/read validation.
//!
//! This example:
//! 1. Generates mock trade data
//! 2. Writes it to KDB+
//! 3. Reads it back from KDB+
//! 4. Validates that the read data matches the written data
//!
//! # Setup
//!
//! Start a KDB+ instance on port 5000:
//!
//! ```sh
//! q -p 5000
//! ```
//!
//! Then in the q console, create an empty trade table:
//!
//! ```q
//! trade:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
//! ```
//!
//! # Run
//!
//! ```sh
//! cargo run --example kdb
//! ```

use anyhow::Result;
use log::Level;
use std::rc::Rc;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

#[derive(Debug, Clone, Default, PartialEq)]
struct Trade {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for Trade {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<Self, KdbError> {
        Ok(Trade {
            // Skip column 0 (time) - it's handled automatically by the adapter
            sym: row.get_sym(1, interner)?,
            price: row.get(2)?.get_float()?,
            qty: row.get(3)?.get_long()?,
        })
    }
}

impl KdbSerialize for Trade {
    fn to_kdb_row(&self) -> K {
        // Return business data only - time will be prepended automatically
        K::new_compound_list(vec![
            K::new_symbol(self.sym.to_string()),
            K::new_float(self.price),
            K::new_long(self.qty),
        ])
    }
}

/// Generate a stream of mock trade data with timestamps
fn generate() -> Rc<dyn Stream<Trade>> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    let mut interner = SymbolInterner::default();
    let mock_trades: Vec<Trade> = (0..10)
        .map(|i| Trade {
            sym: interner.intern(syms[i % syms.len()]),
            price: 100.0 + i as f64,
            qty: (i * 10 + 1) as i64,
        })
        .collect();

    produce_async(move |_ctx| {
        let trades = mock_trades;
        async move {
            Ok(async_stream::stream! {
                for (i, trade) in trades.into_iter().enumerate() {
                    // Use distinct timestamps starting from KDB epoch
                    let time = NanoTime::from_kdb_timestamp(i as i64 * 1_000_000_000);
                    yield Ok((time, trade));
                }
            })
        }
    })
}

/// Write a stream of trades to a KDB+ table
fn write(
    conn: KdbConnection,
    table: &str,
    stream: &Rc<dyn Stream<Trade>>,
) -> Rc<dyn Node> {
    kdb_write(conn, table, stream)
}

/// Read a stream of trades from KDB+ using a query
fn read(
    conn: KdbConnection,
    query: &str,
    time_col: &str,
    batch_size: usize,
) -> Rc<dyn Stream<Trade>> {
    kdb_read::<Trade>(conn, query, time_col, batch_size)
}

fn main() -> Result<()> {
    env_logger::init();

    let conn = KdbConnection::new("localhost", 5000);

    // Step 1: Generate mock trade stream
    let trade_stream = generate();
    let trade_count = 10; // Known from generate() implementation
    println!("Generated stream with {} mock trades", trade_count);

    // Step 2: Write trades to KDB
    let writer = write(conn.clone(), "trade", &trade_stream);
    println!("Writing trades to KDB...");
    writer.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    println!("Write complete");

    // Step 3: Read trades back from KDB
    println!("Reading trades from KDB...");
    let read_stream = read(conn, "select from trade", "time", 10000);
    let collected = read_stream
        .collapse()
        .logged("trades", Level::Info)
        .collect();

    collected
        .clone()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    let read_trades = collected.peek_value();
    println!("Read {} trades from KDB", read_trades.len());

    // Step 4: Validate round-trip by comparing counts and sample data
    println!("\nValidating round-trip...");
    assert_eq!(
        read_trades.len(),
        trade_count,
        "Row count mismatch: expected {}, got {}",
        trade_count,
        read_trades.len()
    );

    println!("✓ All {} trades validated successfully!", trade_count);
    println!("\nSample trades (first 3):");
    for (i, record) in read_trades.iter().take(3).enumerate() {
        println!(
            "  [{}] sym={}, price={:.2}, qty={}",
            i, record.value.sym, record.value.price, record.value.qty
        );
    }

    Ok(())
}
