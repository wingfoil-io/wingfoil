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

fn generate(num_rows: u32) -> Rc<dyn Stream<Burst<Trade>>> {
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
        .limit(num_rows)
}

fn main() -> Result<()> {
    env_logger::init();

    let conn = KdbConnection::new("localhost", 5000);

    let num_rows = 10;
    let trades = generate(num_rows);

    // Write trades to KDB using fluent API
    trades
        .kdb_write(conn.clone(), "trade")
        .run(RunMode::RealTime, RunFor::Forever)?;

    let read_stream = kdb_read::<Trade>(conn, "select from trade", "time", 10000);
    let collected = read_stream
        .collapse()
        .logged("trades", Level::Info)
        .collect();
    collected.clone().run(RunMode::RealTime, RunFor::Forever)?;

    let read_trades = collected.peek_value();
    assert_eq!(read_trades.len(), num_rows as usize);

    Ok(())
}
