#![doc = include_str!("./README.md")]

use anyhow::Result;
use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

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

    kdb_read::<Price, _>(
        conn,
        chunk,
        |(t0, t1), _date, _iter| {
            format!(
                "select time, sym, mid from prices where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                t0.to_kdb_timestamp(),
                t1.to_kdb_timestamp(),
            )
        },
    )
    .logged("prices", Info)
    .run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(Duration::from_secs(100)),
    )?;

    Ok(())
}
