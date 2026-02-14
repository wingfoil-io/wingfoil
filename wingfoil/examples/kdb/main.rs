//! KDB+ adapter example: demonstrates round-trip write/read validation.
//!
//! This example:
//! 1. Generates mock trade data
//! 2. Writes it to KDB+
//! 3. Reads it back from KDB+
//! 4. Validates that the read data matches the generated data
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
use ordered_float::OrderedFloat;
use std::rc::Rc;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

type Price = OrderedFloat<f64>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Trade {
    sym: Sym,
    price: Price,
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
            price: OrderedFloat(row.get(2)?.get_float()?),
            qty: row.get(3)?.get_long()?,
        })
    }
}

impl KdbSerialize for Trade {
    fn to_kdb_row(&self) -> K {
        K::new_compound_list(vec![
            K::new_symbol(self.sym.to_string()),
            K::new_float(self.price.into_inner()),
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
                price: OrderedFloat(100.0 + i as f64),
                qty: (i * 10 + 1) as i64,
            }]
        })
        .limit(num_rows)
}

fn assert_equal<T: Element + Eq>(a: Rc<dyn Stream<T>>, b: Rc<dyn Stream<T>>) -> Rc<dyn Node> {
    bimap(Dep::Active(a), Dep::Active(b), |a, b| assert!(a == b))
}

fn main() -> Result<()> {
    env_logger::init();
    let conn = KdbConnection::new("localhost", 5000);
    let table = "trades";
    let query = "select from trade";
    let time_col = "time";
    let chunk = 10000;
    let num_rows = 10;
    let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
    let run_for = RunFor::Forever;
    generate(num_rows)
        .kdb_write(conn.clone(), table)
        .run(run_mode, run_for)?;
    let baseline = generate(num_rows);
    let read = kdb_read::<Trade>(conn, query, time_col, chunk);
    let assertions = vec![
        assert_equal(read.clone(), baseline.clone()),
        assert_equal(read.count(), baseline.count()),
    ];
    Graph::new(assertions, run_mode, run_for).run()?;
    Ok(())
}
