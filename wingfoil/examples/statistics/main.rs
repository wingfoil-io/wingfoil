//! Streaming statistics: EWMA, rolling windows, and time-weighting.
//!
//! Demonstrates the [`StatisticsOperators`] trait — a family of numeric
//! aggregations that chain onto any `Stream<T: ToPrimitive>` and emit `f64`.
//! Every operator comes in a count-based flavour and, where it makes sense, a
//! time-weighted one (via [`Weighting`]) so irregular tick spacing is handled
//! correctly.
//!
//! One price stream feeds a spread of statistics; the combined row is sampled
//! twice a second and logged as a table.
//!
//! ```bash
//! cargo run --example statistics
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

/// Snapshot several `f64` statistics into one row, in the given order.  Each
/// stat is folded on as a *passive* input — read every cycle, but only the head
/// (derived from the price) drives the row — so column order is explicit rather
/// than dependent on graph structure.
fn snapshot(stats: Vec<Rc<dyn Stream<f64>>>) -> Rc<dyn Stream<Vec<f64>>> {
    let mut stats = stats.into_iter();
    let head = stats.next().expect("snapshot needs at least one statistic");
    stats.fold(head.map(|v| vec![v]), |row, stat| {
        bimap(Dep::Active(row), Dep::Passive(stat), |mut row, v| {
            row.push(v);
            row
        })
    })
}

fn main() {
    // A single synthetic price stream: a gentle sine wave around 100, one
    // sample every 100ms. Every statistic below is derived from this one stream.
    let price = ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0);

    let row = snapshot(vec![
        price.clone(),                  // raw price
        price.ewma(0.3),                // exponential smoothing
        price.rolling_mean(10),         // 10-sample SMA
        price.rolling_std(10),          // 10-sample volatility
        price.rolling_min(10),          // 10-sample low
        price.rolling_max(10),          // 10-sample high
        price.average(Weighting::Time), // time-weighted average (TWAP)
    ]);

    println!(
        "{:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "time", "price", "ewma", "sma", "std", "min", "max", "twap",
    );

    // Sample the row twice a second and log it — the graph runs every 100ms, but
    // we only want a readable trace, not every tick.
    row.sample(ticker(Duration::from_millis(500)))
        .for_each(|r, t| {
            let secs = f64::from(t) / 1e9;
            println!(
                "{:>5.1}s {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2}",
                secs, r[0], r[1], r[2], r[3], r[4], r[5], r[6],
            );
        })
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_secs(5)),
        )
        .unwrap();
}
