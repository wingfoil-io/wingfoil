//! KDB+ adapter example: reads trades, computes running average price, and prints results.
//!
//! # Setup
//!
//! Start a KDB+ instance on port 5000 and create/populate the trade table:
//!
//! ```q
//! q -p 5000
//! ```
//!
//! Then in the q console:
//!
//! ```q
//! trade:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
//! n:100
//! insert[`trade;(2000.01.01D00:00:00.000000000+1000000000*til n;n?`AAPL`GOOG`MSFT;n?100.0;n?1000j)]
//! ```
//!
//! # Run
//!
//! ```sh
//! cargo run --example kdb
//! ```

use log::Level;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
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
            sym: row.get_sym(1, interner)?,
            price: row.get(2)?.get_float()?,
            qty: row.get(3)?.get_long()?,
        })
    }
}

fn main() {
    env_logger::init();

    let conn = KdbConnection::new("localhost", 5000);

    // Read trades from KDB, compute running average price, print
    kdb_read::<Trade>(conn, "select from trade", "time", 10000)
        .collapse()
        .map(|t: Trade| t.price)
        .logged("price", Level::Info)
        .average()
        .logged("avg", Level::Info)
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
}
