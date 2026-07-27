#![doc = include_str!("./README.md")]

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::kdb::{
    CacheConfig, KdbConnection, KdbDeserialize, KdbError, Row, Sym, SymbolInterner, kdb_read_cached,
};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

// `kdb_read_cached` requires `T: Serialize + Deserialize + Sync` in addition to
// `KdbDeserialize`, so the cached slices can be written to / read from disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    let start = NanoTime::from_kdb_timestamp(0);
    let run_for = RunFor::Duration(Duration::from_secs(3600));
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for,
        start_time: start,
    };

    // 512 MiB cap; the first run populates the cache, later runs serve from disk
    // without opening a KDB connection (delete the folder or `CacheConfig::clear()`
    // to force a refetch — bincode is not schema-evolution safe).
    let cache = CacheConfig::new("/tmp/wingfoil-kdb-cache", 512 * 1024 * 1024);

    let g = GraphBuilder::new();
    let _prices = kdb_read_cached::<Price>(
        &g,
        params,
        conn,
        Duration::from_secs(3600),
        cache,
        |(t0, t1), _date, _iter| {
            format!(
                "select time, sym, mid from prices where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                t0.to_kdb_timestamp(),
                t1.to_kdb_timestamp(),
            )
        },
    )?
    .logged("prices", log::Level::Info);

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(start), run_for)?;
    Ok(())
}
