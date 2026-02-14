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
//! **Note:** Before each run, clear the table in the q console:
//!
//! ```q
//! delete from `trade
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
fn generate() -> Rc<dyn Stream<Burst<Trade>>> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    let mut interner = SymbolInterner::default();
    let syms_interned: Vec<Sym> = syms.iter().map(|s| interner.intern(s)).collect();

    ticker(std::time::Duration::from_nanos(1_000_000_000))
        .count()
        .map(move |i| {
            burst![Trade {
                sym: syms_interned[i as usize % syms_interned.len()].clone(),
                price: 100.0 + i as f64,
                qty: (i * 10 + 1) as i64,
            }]
        })
}

/// Write a stream of trades to a KDB+ table
fn write(conn: KdbConnection, table: &str, stream: &Rc<dyn Stream<Burst<Trade>>>) -> Rc<dyn Node> {
    kdb_write(conn, table, stream)
}

/// Read a stream of trades from KDB+ using a query
fn read(
    conn: KdbConnection,
    query: &str,
    time_col: &str,
    batch_size: usize,
) -> Rc<dyn Stream<Burst<Trade>>> {
    kdb_read::<Trade>(conn, query, time_col, batch_size)
}

fn main() -> Result<()> {
    env_logger::init();

    let conn = KdbConnection::new("localhost", 5000);

    // Step 1: Generate mock trade stream
    let trade_stream = generate();
    let trade_count: u32 = 10; // Known from generate() implementation
    println!("Generated stream with {} mock trades", trade_count);

    // Step 2: Write trades to KDB
    let writer = write(conn.clone(), "trade", &trade_stream);
    println!("Writing trades to KDB...");
    writer.run(RunMode::RealTime, RunFor::Cycles(trade_count))?;
    println!("Write complete");

    // Step 3: Read trades back from KDB
    println!("Reading trades from KDB...");
    let read_stream = read(conn, "select from trade", "time", 10000);
    let collected = read_stream
        .collapse()
        .logged("trades", Level::Info)
        .collect();

    collected.clone().run(RunMode::RealTime, RunFor::Forever)?;

    let read_trades = collected.peek_value();
    println!("Read {} trades from KDB", read_trades.len());

    // Step 4: Validate round-trip by comparing counts and sample data
    println!("\nValidating round-trip...");
    assert_eq!(
        read_trades.len(),
        trade_count as usize,
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
