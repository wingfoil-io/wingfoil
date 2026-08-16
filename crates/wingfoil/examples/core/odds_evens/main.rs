//! Odds and evens — the split-and-recombine diamond in the **builder-less**
//! style: `ticker` is a free source function and the stream runs directly, with
//! no `GraphBuilder` or `Runner` in hand. A counter fans out by parity into two
//! labelled branches, which `merge` recombines, tapped with `logged`.
//!
//! The graph is wired once and run either way — deterministic replay by
//! default, or paced by the wall clock with `-- realtime`. Only the [`RunMode`]
//! differs; the wiring above it is the same in both.
//!
//! `logged` emits through the `log` crate (carrying the engine time), so run
//! with `RUST_LOG=info` to see the output.
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil --example odds_evens
//! RUST_LOG=info cargo run -p wingfoil --example odds_evens -- realtime
//! ```

use std::time::Duration;

use wingfoil::signal::ticker;
use wingfoil::{NanoTime, RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    env_logger::init();

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

    // `count` is the shared apex node: both branches read it, and the engine
    // runs it once per cycle, fanning the tick out to each reader.
    let count = ticker(Duration::from_millis(10)).count();

    let evens = count
        .filter(&count.map(|i| i % 2 == 0))
        .map(|i| format!("{i} is even"));
    let odds = count
        .filter(&count.map(|i| i % 2 == 1))
        .map(|i| format!("{i} is odd"));

    odds.merge(&evens)
        .logged("odds/evens", log::Level::Info)
        .run(run_mode, RunFor::Cycles(6))
}
