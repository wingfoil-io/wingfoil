//! Smallest wingfoil-next graph: a ticker counted and formatted, run in
//! historical mode (instant, deterministic) and then in realtime.
//!
//! ```sh
//! cargo run -p wingfoil-next --example hello_graph
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::Builder;

fn main() {
    // Historical: the whole run happens instantly at simulated times.
    let mut g = Builder::new();
    let tick = g.ticker(Duration::from_millis(100));
    let count = g.fold(tick, 0u64, |acc, _: &()| *acc += 1);
    let msgs = g.map(count, |i: &u64| format!("tick {i}"));
    let acc = g.fold(msgs, Vec::new(), |acc: &mut Vec<String>, v: &String| {
        acc.push(v.clone())
    });
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5));
    println!("historical run (instant):");
    for msg in runner.value(acc) {
        println!("  {msg}");
    }

    // Realtime: the same wiring, but the kernel waits out each 50ms tick on
    // the wall clock.
    let mut g = Builder::new();
    let tick = g.ticker(Duration::from_millis(50));
    let count = g.fold(tick, 0u64, |acc, _: &()| *acc += 1);
    let mut runner = g.build();
    println!("realtime run (3 ticks, 50ms apart):");
    runner.run(RunMode::RealTime, RunFor::Cycles(3));
    println!("  counted {} ticks", runner.value(count));
}
