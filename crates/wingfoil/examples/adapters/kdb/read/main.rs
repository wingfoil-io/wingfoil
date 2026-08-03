#![doc = include_str!("./README.md")]

use anyhow::Result;
use std::time::Duration;
use wingfoil::adapters::kdb::{
    KdbConnection, KdbDeserialize, KdbError, Row, Sym, SymbolInterner, kdb_read,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct Price {
    sym: Sym,
    mid: f64,
}

impl KdbDeserialize for Price {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((
            time,
            Price {
                sym: row.get_sym(1, interner)?,
                mid: row.get(2)?.get_float()?,
            },
        ))
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let conn = KdbConnection::new("localhost", 5000);
    let chunk = Duration::from_secs(10);
    let start = NanoTime::from_kdb_timestamp(0);
    let run_for = RunFor::Duration(Duration::from_secs(100));
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for,
        start_time: start,
    };

    let g = GraphBuilder::new();
    let _prices = kdb_read::<Price>(
        &g,
        params,
        conn,
        chunk,
        |(t0, t1), _date, _iter| {
            format!(
                "select time, sym, mid from prices where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                t0.to_kdb_timestamp(),
                t1.to_kdb_timestamp(),
            )
        },
        None,
    )?
    .logged("prices", log::Level::Info);

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(start), run_for)?;
    Ok(())
}
