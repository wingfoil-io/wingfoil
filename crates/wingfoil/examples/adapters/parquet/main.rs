//! The Parquet adapter end to end: archive a stream into a Hive-partitioned
//! tree of `.parquet` files (one directory per UTC day), list the tree, then
//! replay the whole tree back as one stream at the graph times that wrote it.
//! Run with the `parquet` feature:
//!
//! ```sh
//! cargo run -p wingfoil --features parquet --example parquet_adapter
//! ```

use std::path::Path;
use std::time::Duration;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use wingfoil::adapters::parquet::{ParquetSinkOps, TimePartition, parquet_read};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// What the graph produces — serde field names become the Parquet columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Trade {
    seq: u64,
    px: f64,
    qty: u32,
}

/// What we read back: the sink's leading `time` column (a nanosecond
/// `Timestamp`, read as `i64` nanoseconds since the epoch), then `Trade`'s
/// fields. Naming `time` here is what lets the replay run at the original
/// graph times.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Archived {
    time: i64,
    seq: u64,
    px: f64,
    qty: u32,
}

fn main() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join("wingfoil_parquet_adapter");
    // Start clean so the listing below shows only this run's partitions.
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }

    // Archive: a trade every 8 hours from 2026-10-02T04:00Z, partitioned by day.
    let start = NanoTime::new(1_790_913_600_000_000_000);
    let g = GraphBuilder::new();
    let trades = g
        .ticker(Duration::from_secs(8 * 3600))
        .count()
        .map(|&n| Trade {
            seq: n,
            px: 100.0 + n as f64 * 0.25,
            qty: 10 * n as u32,
        });
    let _sink = trades.parquet_write_partitioned(&root, TimePartition::Day)?;
    g.build()
        .run(RunMode::HistoricalFrom(start), RunFor::Cycles(7))?;

    println!("wrote {}:", root.display());
    for file in files_under(&root)? {
        println!("  {file}");
    }

    // Replay: the whole tree as one stream, each row at its archived time.
    let g = GraphBuilder::new();
    let _print = parquet_read(&g, &root, |a: &Archived| NanoTime::new(a.time as u64))?
        .with_time()
        .for_each(|(time, burst)| {
            for a in burst.iter() {
                println!(
                    "{}  seq={} px={:.2} qty={}",
                    NaiveDateTime::from(*time),
                    a.seq,
                    a.px,
                    a.qty
                );
            }
            Ok(())
        });
    println!("replayed:");
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    Ok(())
}

/// Every file under `root`, relative to it, sorted.
fn files_under(root: &Path) -> anyhow::Result<Vec<String>> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, root, out)?;
            } else {
                out.push(path.strip_prefix(root)?.display().to_string());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}
