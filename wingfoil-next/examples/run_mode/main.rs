#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil-next --example run_mode -- realtime
//! cargo run -p wingfoil-next --example run_mode -- historical
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::Stream;
use wingfoil_next::prelude::*;

// ── trait ───────────────────────────────────────────────────────────────────

/// A source of market data. The default `price` impl is a mock: a ticker that
/// emits a synthetic price cycling through 100.0 – 109.0. A real deployment
/// would override it (a socket in realtime, a file replay in historical); the
/// graph wiring downstream never changes.
trait MarketDataBuilder {
    fn price(&self, g: &GraphBuilder) -> Stream<f64> {
        g.ticker(Duration::from_nanos(1))
            .count()
            .map(|n| 100.0 + (n % 10) as f64)
    }
}

// ── implementations ─────────────────────────────────────────────────────────

struct RealTimeMarketDataBuilder;
struct HistoricalMarketDataBuilder;

impl MarketDataBuilder for RealTimeMarketDataBuilder {}
impl MarketDataBuilder for HistoricalMarketDataBuilder {}

// ── run ─────────────────────────────────────────────────────────────────────

fn run(run_mode: RunMode) -> anyhow::Result<()> {
    // Select the appropriate builder for this run mode.
    let builder: Box<dyn MarketDataBuilder> = match run_mode {
        RunMode::RealTime => Box::new(RealTimeMarketDataBuilder),
        RunMode::HistoricalFrom(_) => Box::new(HistoricalMarketDataBuilder),
    };

    // Build the graph — add business logic here. It is identical in both modes.
    let g = GraphBuilder::new();
    let prices = builder.price(&g);

    let mut runner = g.build();
    runner.run(run_mode, RunFor::Cycles(5))?;
    println!("last price: {}", runner.value(&prices));
    Ok(())
}

// ── main ────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "historical".into())
        .to_lowercase();
    let run_mode = match arg.as_str() {
        "realtime" => RunMode::RealTime,
        "historical" => RunMode::HistoricalFrom(NanoTime::ZERO),
        other => {
            eprintln!("unknown run mode: {other:?}. Use 'realtime' or 'historical'.");
            std::process::exit(1);
        }
    };
    run(run_mode)
}
