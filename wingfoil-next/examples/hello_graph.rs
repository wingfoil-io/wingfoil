//! Smallest wingfoil-next graph, in the fluent style: a ticker counted and
//! formatted, run in historical mode (instant, deterministic) and then in
//! realtime.
//!
//! ```sh
//! cargo run -p wingfoil-next --example hello_graph
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;

fn main() {
    // Historical: the whole run happens instantly at simulated times.
    let g = GraphBuilder::new();
    let msgs = g
        .ticker(Duration::from_millis(100))
        .count()
        .map(|i| format!("tick {i}"))
        .accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();
    println!("historical run (instant):");
    for msg in runner.value(&msgs) {
        println!("  {msg}");
    }

    // Realtime: the same wiring, but the kernel waits out each 50ms tick on
    // the wall clock.
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_millis(50)).count();
    let mut runner = g.build();
    println!("realtime run (3 ticks, 50ms apart):");
    runner.run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();
    println!("  counted {} ticks", runner.value(&count));
}
