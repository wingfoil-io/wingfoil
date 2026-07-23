#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil-next --example breadth_first
//! ```

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // A single source, then 127 diamonds stacked on top of it: each
    // `join(&source, &source)` reads the *same* upstream node twice and sums —
    // branch then recombine. The two inputs share one node index, so the
    // engine visits it once per tick regardless of how many paths lead in.
    let mut source = g.constant(1_u128);
    for _ in 1..128 {
        source = source.join(&source, |a, b| a + b);
    }

    // `.timed()` prints a one-line perf summary at teardown.
    let out = source.timed();

    let mut runner = g.build();
    // The constant fires once; nothing reschedules, so `Forever` self-
    // terminates after a single tick — the whole 127-deep DAG in one cycle.
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    // 2^127 — the correct answer a depth-first engine would need 2^127 visits
    // to reach.
    println!("value {:?}", runner.value(&out));
    Ok(())
}
