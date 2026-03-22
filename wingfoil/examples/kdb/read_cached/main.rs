#![doc = include_str!("./README.md")]

use anyhow::Result;
use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

// serde derives are required by kdb_read_cached
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

fn run(conn: KdbConnection, config: CacheConfig) -> Result<()> {
    kdb_read_cached::<Price, _>(
        conn,
        Duration::from_secs(10),
        config,
        |(t0, t1), _date, _iter| {
            format!(
                "select time, sym, mid from prices \
                 where time >= (`timestamp$){}j, time < (`timestamp$){}j",
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

fn main() -> Result<()> {
    env_logger::init();

    // 10 MiB cap — oldest cache files are evicted when the limit is exceeded.
    // Use u64::MAX for an unbounded cache.
    let config = CacheConfig::new("/tmp/wingfoil-kdb-cache", 10 * 1024 * 1024);

    println!("Run 1: cache miss — queries KDB and writes cache files");
    run(KdbConnection::new("localhost", 5000), config.clone())?;

    println!("Run 2: cache hit — no KDB connection needed");
    // Point at a closed port: if every slice is a cache hit the run still succeeds.
    run(KdbConnection::new("localhost", 59999), config.clone())?;

    println!("Clearing cache");
    config.clear()?;

    println!("Done");
    Ok(())
}
