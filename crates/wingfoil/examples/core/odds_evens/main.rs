//! Odds and evens — the split-and-recombine diamond: a counter fans out by
//! parity into two labelled branches, which `merge` recombines, tapped with
//! `logged`. Wiring, building and running are the three explicit steps every
//! wingfoil program takes: wire streams from a [`GraphBuilder`], `build()` the
//! graph, `run(..)` the runner.
//!
//! `logged` emits through the `log` crate (carrying the engine time), so run
//! with `RUST_LOG=info` to see the output.
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil --example odds_evens
//! ```

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let g = GraphBuilder::new();

    // `count` is the shared apex node: both branches read it, and the engine
    // runs it once per cycle, fanning the tick out to each reader.
    let count = g.ticker(Duration::from_millis(10)).count();

    let evens = count
        .filter(&count.map(|i| i % 2 == 0))
        .map(|i| format!("{i} is even"));
    let odds = count
        .filter(&count.map(|i| i % 2 == 1))
        .map(|i| format!("{i} is odd"));

    odds.merge(&evens).logged("odds/evens", log::Level::Info);

    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
}
