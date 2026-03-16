#![doc = include_str!("./README.md")]

use anyhow::Result;
use log::Level;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

#[derive(Debug, Clone, Default)]
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

    let prices = kdb_read::<Price, _>(
        conn,
        std::time::Duration::from_secs(86400),
        |(_t0, _t1), _date, _iter| "select time, sym, mid from prices".to_string(),
    );

    let run_mode = RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0));
    let run_for = RunFor::Duration(std::time::Duration::from_secs(1));

    prices
        .logged("prices", Level::Info)
        .run(run_mode, run_for)?;

    Ok(())
}
