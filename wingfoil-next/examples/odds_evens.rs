//! The canonical odds/evens graph in the fluent style: a counter is split by
//! parity into two labelled branches, then merged back into one stream — the
//! textbook "split and recombine" DAG. It's the graph the parity tests use,
//! shown here as a runnable program.
//!
//! Written once as a `graph!` wiring function, it expands to both engines:
//! `interpreted()` (dynamic, shared-node) and `compiled()` (fully
//! monomorphized straight-line code). This example runs both and checks they
//! produce identical output — the dual-mode guarantee.
//!
//! ```sh
//! cargo run -p wingfoil-next --example odds_evens
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(10);

// One wiring definition. `count` is a *shared* node: both branches read it,
// and the interpreted engine executes it once per cycle, fanning the tick out
// to both `filter`s. `merge` recombines the two branches — at most one fires
// on any given tick, since a number is either odd or even.
wingfoil_next::graph! {
    fn odds_evens(g: &GraphBuilder) -> Stream<Vec<String>> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        let is_odd = is_even.map(|b| !b);
        let odd_str = count.filter(&is_odd).map(|i| format!("{i} is odd"));
        let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
        let acc = odd_str.merge(&even_str).accumulate();
        acc
    }
}

fn main() {
    let run_for = RunFor::Cycles(10);

    // Interpreted: build the graph, run it, read the accumulated labels.
    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);

    // Compiled: the same tokens, monomorphized into a standalone runner.
    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for).unwrap();

    assert_eq!(interpreted, compiled, "both engines must agree");

    for line in &interpreted {
        println!("{line}");
    }
    println!(
        "\n{} labels — interpreted and compiled engines agree.",
        interpreted.len()
    );
}
