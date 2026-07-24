//! The `benches/graph.rs` "10x10" fan-out graph expressed through the
//! `graph!` macro: one `count` source fanned out into 10 parallel 10-deep
//! identity-`map` chains, merged back into one stream.
//!
//! Because `graph!` v1 requires straight-line wiring (the DAG must be static),
//! the 100 `.map()` nodes cannot be built with a `for` loop the way the
//! classic fluent wiring does — they are spelled out literally in the shared
//! `bench_support/fanout_10x10.rs`. From those tokens the macro derives both
//! engines; this example runs both and checks they agree.
//!
//! ```sh
//! cargo run -p wingfoil-next --example fanout_10x10
//! ```

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

include!("../bench_support/fanout_10x10.rs");

fn main() {
    let run_for = RunFor::Cycles(1000);

    // Interpreted: build the graph, run it, read the merged output.
    let (mut runner, out) = fanout::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);

    // Compiled: the same tokens, monomorphized into a standalone runner.
    let (compiled,) = fanout::compiled(HISTORICAL, run_for).unwrap();

    assert_eq!(interpreted, compiled, "both engines must agree");
    println!("10x10 fanout ({run_for:?}): interpreted = compiled = {compiled}");
}
