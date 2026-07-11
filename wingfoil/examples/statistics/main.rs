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

/// Tag a statistic with its column index.  `combine` assembles the row in graph
/// evaluation order, which depends on each stat's depth in the DAG — tagging
/// lets us drop every value into a fixed column regardless.
fn col(index: usize, stat: Rc<dyn Stream<f64>>) -> Rc<dyn Stream<(usize, f64)>> {
    stat.map(move |v| (index, v))
}

fn main() {
    // A single synthetic price stream: a gentle sine wave around 100, one
    // sample every 100ms. Every statistic below is derived from this one stream.
    let price = ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0);

    // Derive a spread of statistics from the shared price stream and combine
    // them into one row per tick.
    let row = combine(vec![
        col(0, price.clone()),                  // raw price
        col(1, price.ewma(0.3)),                // exponential smoothing
        col(2, price.rolling_mean(10)),         // 10-sample SMA
        col(3, price.rolling_std(10)),          // 10-sample volatility
        col(4, price.rolling_min(10)),          // 10-sample low
        col(5, price.rolling_max(10)),          // 10-sample high
        col(6, price.average(Weighting::Time)), // time-weighted average (TWAP)
    ]);

    println!(
        "{:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "time", "price", "ewma", "sma", "std", "min", "max", "twap",
    );

    // Sample the row twice a second and log it — the graph runs every 100ms, but
    // we only want a readable trace, not every tick.
    row.sample(ticker(Duration::from_millis(500)))
        .for_each(|cells, t| {
            let mut c = [f64::NAN; 7];
            for (i, v) in cells {
                c[i] = v;
            }
            let secs = f64::from(t) / 1e9;
            println!(
                "{:>5.1}s {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2}",
                secs, c[0], c[1], c[2], c[3], c[4], c[5], c[6],
            );
        })
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_secs(5)),
        )
        .unwrap();
}
