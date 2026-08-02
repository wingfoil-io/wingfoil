//! Demonstrates the `RunMode` pattern: swap between real-time and historical
//! execution by selecting a different [`MarketDataBuilder`] implementation.
//!
//! Both builders share the same default [`MarketDataBuilder::price`] mock, so
//! the graph wiring is identical regardless of run mode.
//!
//! ```bash
//! cargo run --example run_mode -- realtime
//! cargo run --example run_mode -- historical
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

// ── trait ─────────────────────────────────────────────────────────────────────

trait MarketDataBuilder {
    /// A stream of prices.  The default impl is a mock: a ticker that emits a
    /// synthetic price cycling through 100.0 – 109.0.
    fn price(&self) -> Rc<dyn Stream<f64>> {
        ticker(Duration::from_nanos(1))
            .count()
            .map(|n: u64| 100.0 + (n % 10) as f64)
    }
}

// ── implementations ───────────────────────────────────────────────────────────

struct RealTimeMarketDataBuilder;
struct HistoricalMarketDataBuilder;

impl MarketDataBuilder for RealTimeMarketDataBuilder {}
impl MarketDataBuilder for HistoricalMarketDataBuilder {}

// ── run ───────────────────────────────────────────────────────────────────────

fn run(run_mode: RunMode) {
    // Select the appropriate builder for this run mode.
    let builder: Box<dyn MarketDataBuilder> = match run_mode {
        RunMode::RealTime => Box::new(RealTimeMarketDataBuilder),
        RunMode::HistoricalFrom(_) => Box::new(HistoricalMarketDataBuilder),
    };

    // Build the graph — add business logic here.
    let prices = builder.price();

    prices.run(run_mode, RunFor::Cycles(5)).unwrap();
    println!("last price: {}", prices.peek_value());
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
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
    run(run_mode);
}
