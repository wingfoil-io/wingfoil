//! Streaming statistics: EWMA, rolling windows, and time-weighting.
//!
//! Demonstrates the [`StatisticsOperators`] trait — a family of numeric
//! aggregations that chain onto any `Stream<T: ToPrimitive>` and emit `f64`.
//! Every operator comes in a count-based flavour and, where it makes sense, a
//! time-weighted one (via [`Weighting`]) so irregular tick spacing is handled
//! correctly.
//!
//! ```bash
//! cargo run --example statistics
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

/// A synthetic price series: a gentle sine wave around 100, one sample every
/// 100ms.  A fresh graph is built per call so each statistic below can be run
/// and printed independently.
fn prices() -> Rc<dyn Stream<f64>> {
    ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0)
}

/// Run a single statistic to completion and return its final value.
fn final_value(stat: Rc<dyn Stream<f64>>) -> f64 {
    stat.clone()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();
    stat.peek_value()
}

fn main() {
    println!("30 samples of a synthetic price series (~100 ± 5):\n");

    // ── smoothing ────────────────────────────────────────────────────────────
    println!(
        "ewma(0.2)                          = {:.4}",
        final_value(prices().ewma(0.2))
    );
    println!(
        "ewma_decay(300ms)                  = {:.4}",
        final_value(prices().ewma_decay(Duration::from_millis(300)))
    );

    // ── cumulative: count vs time weighted ───────────────────────────────────
    println!(
        "average(Count)                     = {:.4}",
        final_value(prices().average(Weighting::Count))
    );
    println!(
        "average(Time)                      = {:.4}",
        final_value(prices().average(Weighting::Time))
    );
    println!(
        "std(Count)                         = {:.4}",
        final_value(prices().std(Weighting::Count))
    );

    // ── count-windowed (last N samples) ──────────────────────────────────────
    println!(
        "rolling_mean(5)                    = {:.4}",
        final_value(prices().rolling_mean(5))
    );
    println!(
        "rolling_std(5)                     = {:.4}",
        final_value(prices().rolling_std(5))
    );
    println!(
        "rolling_min(5)                     = {:.4}",
        final_value(prices().rolling_min(5))
    );
    println!(
        "rolling_max(5)                     = {:.4}",
        final_value(prices().rolling_max(5))
    );
    println!(
        "rolling_median(5)                  = {:.4}",
        final_value(prices().rolling_median(5))
    );

    // ── time-windowed (last N of graph time) ─────────────────────────────────
    let win = Duration::from_millis(500);
    println!(
        "rolling_mean_over(500ms, Count)    = {:.4}",
        final_value(prices().rolling_mean_over(win, Weighting::Count))
    );
    println!(
        "rolling_mean_over(500ms, Time)     = {:.4}",
        final_value(prices().rolling_mean_over(win, Weighting::Time))
    );
    println!(
        "rolling_std_over(500ms, Count)     = {:.4}",
        final_value(prices().rolling_std_over(win, Weighting::Count))
    );
}
