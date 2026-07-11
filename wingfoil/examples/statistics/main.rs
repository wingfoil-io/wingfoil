//! Streaming statistics: EWMA, rolling windows, and time-weighting.
//!
//! Demonstrates the [`StatisticsOperators`] trait — a family of numeric
//! aggregations that chain onto any `Stream<T: ToPrimitive>` and emit `f64`.
//! Every operator comes in a count-based flavour and, where it makes sense, a
//! time-weighted one (via [`Weighting`]) so irregular tick spacing is handled
//! correctly.
//!
//! One price stream feeds a spread of statistics; each is sampled twice a
//! second, collected, and printed as a table.
//!
//! ```bash
//! cargo run --example statistics
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

fn main() {
    // A single synthetic price stream: a gentle sine wave around 100, one
    // sample every 100ms. Every statistic below is derived from this one stream.
    let price = ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0);

    // One labelled statistic per column, all derived from the shared price.
    let columns: Vec<(&str, Rc<dyn Stream<f64>>)> = vec![
        ("price", price.clone()),
        ("ewma", price.ewma(0.3)),
        ("sma", price.rolling_mean(10)),
        ("std", price.rolling_std(10)),
        ("min", price.rolling_min(10)),
        ("max", price.rolling_max(10)),
        ("twap", price.average(Weighting::Time)),
    ];

    // Sample each column twice a second and collect it for reading after the run.
    let trigger = ticker(Duration::from_millis(500));
    let collected: Vec<Rc<dyn Stream<Vec<ValueAt<f64>>>>> = columns
        .iter()
        .map(|(_, stat)| stat.sample(trigger.clone()).collect())
        .collect();

    let nodes: Vec<Rc<dyn Node>> = collected.iter().map(|c| c.clone().as_node()).collect();
    Graph::new(
        nodes,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(Duration::from_secs(5)),
    )
    .run()
    .unwrap();

    // Transpose the per-column series into rows and print the table. Every
    // column was sampled on the same trigger, so they line up index-for-index.
    print!("{:>6}", "time");
    for (label, _) in &columns {
        print!(" {label:>7}");
    }
    println!();

    let series: Vec<Vec<ValueAt<f64>>> = collected.iter().map(|c| c.peek_value()).collect();
    for i in 0..series[0].len() {
        print!("{:>5.1}s", f64::from(series[0][i].time) / 1e9);
        for column in &series {
            print!(" {:>7.2}", column[i].value);
        }
        println!();
    }
}
