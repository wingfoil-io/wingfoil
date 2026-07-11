//! Streaming statistics: EWMA, rolling windows, and time-weighting.
//!
//! Demonstrates the [`StatisticsOperators`] trait — a family of numeric
//! aggregations that chain onto any `Stream<T: ToPrimitive>` and emit `f64`.
//! Every operator comes in a count-based flavour and, where it makes sense, a
//! time-weighted one (via [`Weighting`]) so irregular tick spacing is handled
//! correctly.
//!
//! One price stream feeds a spread of statistics.  [`combine`] bundles them into
//! a single `Stream<Burst<f64>>` — one row of values per tick — which we sample
//! twice a second and print as a table row as it ticks.
//!
//! ```bash
//! cargo run --example statistics
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::adapters::statistics::*;
use wingfoil::*;

fn main() {
    // A single synthetic price stream: a gentle sine wave around 100, one
    // sample every 100ms. Every statistic below is derived from this one stream.
    let price = ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0);

    // One statistic per column, all derived from the shared price stream.
    let labels = ["price", "ewma", "sma", "std", "min", "max", "twap"];
    let columns: Vec<Rc<dyn Stream<f64>>> = vec![
        price.clone(),
        price.ewma(EwmaSpan::PerTick(0.3)),
        price.rolling_mean(10, Weighting::Count),
        price.rolling_std(10, Weighting::Count),
        price.rolling_min(10),
        price.rolling_max(10),
        price.average(Weighting::Time),
    ];

    // Header first, then stream: `combine` collects the columns (in order) into
    // one Stream<Burst<f64>> whose value each tick is a row holding every
    // column's latest value. Sample that row twice a second and print it live.
    print!("{:>6}", "time");
    for label in labels {
        print!(" {label:>7}");
    }
    println!();

    let trigger = ticker(Duration::from_millis(500));
    let table = combine(columns).sample(trigger).for_each(|row, time| {
        print!("{:>5.1}s", f64::from(time) / 1e9);
        for value in &row {
            print!(" {value:>7.2}");
        }
        println!();
    });

    Graph::new(
        vec![table],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(Duration::from_secs(5)),
    )
    .run()
    .unwrap();
}
