//! Smallest wingfoil graph, in the fluent style: a ticker counted and
//! formatted, run in historical mode (instant, deterministic) and then in
//! realtime.
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example hello_graph
//! ```

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

fn main() {
    // Historical: the whole run happens instantly at simulated times.
    let g = GraphBuilder::new();
    // `for_each` is the graph's outbound edge: a side-effecting sink that runs
    // per tick, so each message is printed as it is produced rather than piled
    // into a `Vec` to be read after the run. (`.print()` is the one-call debug
    // version, printing `{value:?}`.)
    let _printed = g
        .ticker(Duration::from_millis(100))
        .count()
        .map(|i| format!("tick {i}"))
        .for_each(|msg: &String| {
            println!("  {msg}");
            Ok(())
        });
    let mut runner = g.build();
    println!("historical run (instant):");
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();

    // Realtime: the same wiring, but the kernel waits out each 50ms tick on
    // the wall clock.
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_millis(50)).count();
    let mut runner = g.build();
    println!("realtime run (3 ticks, 50ms apart):");
    runner.run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();
    println!("  counted {} ticks", runner.value(&count));
}
