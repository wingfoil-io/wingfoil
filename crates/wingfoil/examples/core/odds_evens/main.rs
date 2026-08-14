//! Odds and evens — the split-and-recombine diamond in the **builder-less**
//! style: `ticker` is a free source function and the stream runs directly, with
//! no `GraphBuilder` or `Runner` in hand. A counter fans out by parity into two
//! labelled branches, which `merge` recombines, tapped with `logged`.
//!
//! `logged` emits through the `log` crate (carrying the engine time), so run
//! with `RUST_LOG=info` to see the output.
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil --example odds_evens
//! ```

use std::time::Duration;

use wingfoil::signal::ticker;
use wingfoil::{NanoTime, RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    env_logger::init();

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
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
}
