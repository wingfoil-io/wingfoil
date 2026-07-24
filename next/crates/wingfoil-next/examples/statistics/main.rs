#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil-next --example statistics
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // A single synthetic price stream: a gentle sine wave around 100, one
    // sample every 100ms. Every statistic below is derived from this one stream.
    let price = g
        .ticker(Duration::from_millis(100))
        .count()
        .map(|n| 100.0 + ((*n as f64) * 0.6).sin() * 5.0);

    // One statistic per column, all derived from the shared price stream. The
    // `StatisticsOps` trait (opt-in — imported separately from the prelude)
    // supplies EWMA and rolling-window ops on any `Stream<f64>`.
    let ewma = price.ewma_per_tick(0.3);
    let sma = price.rolling_mean(10);
    let std = price.rolling_std(10);
    let lo = price.rolling_min(10);
    let hi = price.rolling_max(10);

    // Bundle the columns into one row per tick. `join` ticks when either input
    // ticks; since every column derives from `price` they all advance on the
    // same cycle, so each row is internally consistent.
    let push = |row: &Vec<f64>, x: &f64| {
        let mut row = row.clone();
        row.push(*x);
        row
    };
    let row = price
        .join(&ewma, |p, e| vec![*p, *e])
        .join(&sma, push)
        .join(&std, push)
        .join(&lo, push)
        .join(&hi, push);

    // Print a header, then sample the row twice a second and print it live.
    let labels = ["price", "ewma", "sma", "std", "min", "max"];
    print!("{:>6}", "time");
    for label in labels {
        print!(" {label:>7}");
    }
    println!();

    let trigger = g.ticker(Duration::from_millis(500));
    let _out = row.with_time().sample(&trigger).for_each(|(time, values)| {
        print!("{:>5.1}s", f64::from(*time) / 1e9);
        for value in values {
            print!(" {value:>7.2}");
        }
        println!();
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(Duration::from_secs(5)),
    )?;
    Ok(())
}
