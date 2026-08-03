#![doc = include_str!("./README.md")]

use anyhow::Result;
use std::time::Duration;
use wingfoil::adapters::kdb::{
    K, KdbConnection, KdbDeserialize, KdbError, KdbSerialize, KdbSinkOps, Row, Sym, SymbolInterner,
    kdb_read,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const SECOND_NANOS: i64 = 1_000_000_000;
const TABLE: &str = "test_trades";

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
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?; // col 0: time
        Ok((
            time,
            Trade {
                sym: row.get_sym(1, interner)?,
                price: row.get(2)?.get_float()?,
                qty: row.get(3)?.get_long()?,
            },
        ))
    }
}

impl KdbSerialize for Trade {
    fn to_kdb_row(&self) -> K {
        // Business data only — the graph timestamp is prepended by kdb_write.
        K::new_compound_list(vec![
            K::new_symbol(self.sym.to_string()),
            K::new_float(self.price),
            K::new_long(self.qty),
        ])
    }
}

/// Deterministic sample trades, one per second from the KDB epoch (2000-01-01).
fn trades(n: i64) -> Vec<(NanoTime, Trade)> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    let mut interner = SymbolInterner::default();
    let syms: Vec<Sym> = syms.iter().map(|s| interner.intern(s)).collect();
    (0..n)
        .map(|i| {
            (
                NanoTime::from_kdb_timestamp(i * SECOND_NANOS),
                Trade {
                    sym: syms[i as usize % syms.len()].clone(),
                    price: 100.0 + i as f64,
                    qty: i * 10 + 1,
                },
            )
        })
        .collect()
}

fn main() -> Result<()> {
    env_logger::init();
    let conn = KdbConnection::new("localhost", 5000);
    let num_rows = 10;
    let start = NanoTime::from_kdb_timestamp(0);
    let run_for = RunFor::Duration(Duration::from_secs(num_rows as u64 + 1));

    let baseline = trades(num_rows);

    // Write — replay the generated trades at their timestamps into the table. The
    // sink flushes at teardown, so scope the runner before reading back.
    {
        let g = GraphBuilder::new();
        let rows = baseline
            .iter()
            .cloned()
            .map(|(time, trade)| Ok((trade, time)));
        let _sink = g
            .replay_results(rows)
            .kdb_write(conn.clone(), TABLE, None)?;
        let mut runner = g.build();
        runner.run(RunMode::HistoricalFrom(start), run_for)?;
    }

    // Read — one 24h slice covers the run.
    let g = GraphBuilder::new();
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(Duration::from_secs(86400)),
        start_time: start,
    };
    let read = kdb_read::<Trade>(
        &g,
        params,
        conn,
        Duration::from_secs(86400),
        move |(t0, t1), _date, _iter| {
            format!(
                "select from {TABLE} where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                t0.to_kdb_timestamp(),
                t1.to_kdb_timestamp(),
            )
        },
        None,
    )?
    .collapse()
    .accumulate();
    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(86400)),
    )?;

    // Tie-out: the read-back trades must equal the generated ones.
    let read: Vec<Trade> = runner.value(&read);
    let expected: Vec<Trade> = baseline.into_iter().map(|(_, trade)| trade).collect();
    assert_eq!(
        read, expected,
        "generated and read data did not tie out (delete the table's prior-run rows first)"
    );

    println!("✓ {num_rows} written, read and validated");
    Ok(())
}
