//! Thread-offload combinators: `spawn` (a producer sub-graph on a worker thread)
//! and `spawn_map` (map an input stream through a worker sub-graph). These are
//! next's ergonomic twins of classic `producer()` / `mapper()` (the `graph_node`
//! node) — the channel + `send_at` + `close` + join plumbing the `threading`
//! example spells out by hand, wrapped as one call each.
//!
//! Both run in both modes. Historical replay is deterministic — the two graphs
//! run lock-step by graph time — so this prints the same numbers every run.
//!
//! ```sh
//! cargo run -p wingfoil-next --example spawn
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

fn main() {
    let period = Duration::from_millis(10);
    let n = 5u32;

    let g = GraphBuilder::new();

    // `spawn`: a producer sub-graph (ticker → running count) runs on its own
    // worker thread and feeds this graph a `Burst` per instant. Flatten the
    // single-value bursts back to a plain stream for the next stage.
    let counts: Stream<Burst<u64>> = g.spawn(move |wg| wg.ticker(period).count().limit(n));
    let flat: Stream<u64> = counts.map(|b: &Burst<u64>| b.iter().sum::<u64>());

    // `spawn_map`: map each value ×10 through a sub-graph on a *second* worker
    // thread. `s` is the input, delivered into the worker over the channel layer.
    let scaled: Stream<Burst<u64>> =
        flat.spawn_map(|s: Stream<Burst<u64>>| s.map(|b: &Burst<u64>| b.iter().sum::<u64>() * 10));

    let collected = scaled.with_time().accumulate();
    let mut runner = g.build();

    // Bound the run; the workers inherit this bound. Historical replay races to
    // the bound with no wall-clock waiting.
    runner
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * (n + 3)),
        )
        .expect("run");

    for (t, v) in runner.value(&collected) {
        println!("{:>3} ms: {:?}", u64::from(t) / 1_000_000, v);
    }
    // 0 ms: [10]
    // 10 ms: [20]
    // 20 ms: [30]
    // 30 ms: [40]
    // 40 ms: [50]
}
