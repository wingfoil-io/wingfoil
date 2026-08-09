#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example introspect
//! ```

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // A shape with something worth looking at: a fast price feed, a slow
    // clock, and a `sample` that reads the price *passively* while the clock
    // triggers it. Which leg is which is invisible in this source — it is the
    // single most common wiring mistake, and it is exactly what the picture
    // shows.
    let price = g.ticker(Duration::from_millis(1)).count();
    let clock = g.ticker(Duration::from_millis(10));
    let sampled = price.sample(&clock);
    let _doubled = sampled.map(|n: &u64| n * 2);

    // The snapshot does not consume the builder, so it can be taken at any
    // point during wiring, as often as you like.
    let snap = g.snapshot();

    // `to_text` is also the `Display` impl — the form to reach for in a test
    // failure or a terminal. `<-` is an active edge, `<~` a passive one.
    println!("{snap}");

    println!(
        "sources: {:?}",
        snap.sources().map(|n| n.index).collect::<Vec<_>>()
    );
    println!(
        "sinks:   {:?}",
        snap.sinks().map(|n| n.index).collect::<Vec<_>>()
    );

    // Mermaid renders inline on GitHub, so this is the form to paste into a
    // README, an issue or a pull request.
    println!("\n--- mermaid ---\n{}", snap.to_mermaid());

    // Graphviz lays out a wide DAG far better than anything else here:
    //   cargo run --example introspect | sed -n '/digraph/,/^}/p' | dot -Tsvg > graph.svg
    println!("--- dot ---\n{}", snap.to_dot());

    // Structure is not values: running the graph does not change the topology,
    // and a snapshot can be taken from the `Runner` afterwards too.
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(50))?;
    assert_eq!(runner.snapshot(), snap);
    println!("after {} cycles the topology is unchanged", 50);

    Ok(())
}
