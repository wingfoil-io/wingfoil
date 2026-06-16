//! Live-demo example: a moving-average **crossover** trading signal.
//!
//! Built entirely from core wingfoil operators — no I/O adapters required —
//! so it is ideal for a from-scratch, live-coded walkthrough.
//!
//! A synthetic mid price feeds two exponential moving averages (EMAs). A fast
//! EMA reacts quickly; a slow EMA lags. When the fast crosses above the slow we
//! are `LONG`; when it crosses back below we go `FLAT`. `distinct()` means the
//! graph only emits on an actual crossover.
//!
//! Run in real time (watch it tick on the wall clock):
//!
//! ```sh
//! cargo run --example crossover
//! ```
//!
//! Then change `RunMode::RealTime` to `RunMode::HistoricalFrom(NanoTime::ZERO)`
//! for an instant backtest over the same graph wiring.

use std::time::Duration;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    let period = Duration::from_millis(20);

    // Synthetic mid price: a gentle upward drift plus an oscillation, so the
    // moving averages cross each other a handful of times.
    let prices = ticker(period)
        .count()
        .map(|n| 100.0 + 15.0 * ((n as f64) / 25.0).sin() + (n as f64) * 0.02);

    // Two exponential moving averages of the price. `fold` carries the running
    // EMA across ticks: new = old + alpha * (price - old). Fast reacts quickly,
    // slow lags behind. We seed each EMA from the first sample (its state starts
    // at 0.0) so they don't have to warm up from zero.
    let ema = |alpha: f64| {
        move |ema: &mut f64, price: f64| {
            *ema = if *ema == 0.0 {
                price
            } else {
                *ema + alpha * (price - *ema)
            };
        }
    };
    let fast = prices.fold(ema(0.30));
    let slow = prices.fold(ema(0.05));

    // Combine all three streams into one human-readable line per tick. `prices`
    // is the active trigger; the two EMAs are passive (read, not triggering),
    // but the graph still orders them before this node. LONG when the fast EMA
    // is above the slow EMA, else FLAT.
    let report = trimap(
        Dep::Active(prices.clone()),
        Dep::Passive(fast.clone()),
        Dep::Passive(slow.clone()),
        |price, fast, slow| {
            let signal = if fast > slow { "^ LONG" } else { "v FLAT" };
            format!("price {price:7.2}   fast {fast:7.2}   slow {slow:7.2}   {signal}")
        },
    );

    // `for_each` runs the closure on every tick and prints immediately, so the
    // output streams live in real time (unlike `print()`, which buffers and
    // flushes at the end).
    report
        .for_each(|line, time| println!("{}   {line}", time.pretty()))
        .run(RunMode::RealTime, RunFor::Cycles(150))?;
    Ok(())
}
