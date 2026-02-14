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

fn main() -> Result<()> {
    env_logger::init();

    let conn = KdbConnection::new("localhost", 5000);

    // Generate mock data: 10 trades with distinct timestamps
    let syms = ["AAPL", "GOOG", "MSFT"];
    let mut interner = SymbolInterner::default();
    let mock_trades: Vec<Trade> = (0..10)
        .map(|i| Trade {
            sym: interner.intern(syms[i % syms.len()]),
            price: 100.0 + i as f64,
            qty: (i * 10 + 1) as i64,
        })
        .collect();

    println!("Generated {} mock trades", mock_trades.len());

    // Step 1: Write mock data to KDB
    let write_conn = conn.clone();
    let trades_to_write = mock_trades.clone();
    let write_stream = produce_async(move |_ctx| {
        let trades = trades_to_write;
        async move {
            Ok(async_stream::stream! {
                for (i, trade) in trades.into_iter().enumerate() {
                    // Use distinct timestamps starting from KDB epoch
                    let time = NanoTime::from_kdb_timestamp(i as i64 * 1_000_000_000);
                    yield Ok((time, trade));
                }
            })
        }
    });

    let writer = kdb_write(write_conn, "trade", &write_stream);
    println!("Writing trades to KDB...");
    writer.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    println!("Write complete");

    // Step 2: Read data back from KDB
    println!("Reading trades from KDB...");
    let read_stream = kdb_read::<Trade>(conn, "select from trade", "time", 10000);
    let collected = read_stream
        .collapse()
        .logged("trades", Level::Info)
        .collect();

    collected
        .clone()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    let read_trades = collected.peek_value();
    println!("Read {} trades from KDB", read_trades.len());

    // Step 3: Validate - compare read data with mock data
    println!("\nValidating round-trip...");
    assert_eq!(
        read_trades.len(),
        mock_trades.len(),
        "Row count mismatch: expected {}, got {}",
        mock_trades.len(),
        read_trades.len()
    );

    let mut all_match = true;
    for (i, read_record) in read_trades.iter().enumerate() {
        let expected = &mock_trades[i];
        let actual = &read_record.value;

        let matches = expected.sym == actual.sym
            && (expected.price - actual.price).abs() < 1e-10
            && expected.qty == actual.qty;

        if !matches {
            eprintln!(
                "Mismatch at index {}: expected {:?}, got {:?}",
                i, expected, actual
            );
            all_match = false;
        }
    }

    if all_match {
        println!("✓ All {} trades validated successfully!", mock_trades.len());
        println!("\nSample comparison (first 3 trades):");
        for (i, record) in read_trades.iter().take(3).enumerate() {
            println!(
                "  [{}] sym={}, price={:.2}, qty={}",
                i, record.value.sym, record.value.price, record.value.qty
            );
        }
        Ok(())
    } else {
        anyhow::bail!("Validation failed: some trades did not match")
    }
}
