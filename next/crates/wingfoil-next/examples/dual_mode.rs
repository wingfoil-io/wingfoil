//! The dual-mode thesis, completed by the `graph!` macro: **one** wiring
//! definition expands to both an interpreted runner and a fully
//! monomorphized compiled runner. The two cannot drift — same tokens, same
//! `Op` semantics — and the compiled one gets the compiler's full
//! optimization across node boundaries.
//!
//! The wiring is a **split/recombine** DAG: a counter is split on parity into
//! two labelled branches which are then merged back into one stream — so this
//! also shows the macro deriving a non-linear graph (a shared apex node and a
//! recombine) for both engines:
//!
//! ```text
//!               count                 (apex — shared node, once/cycle)
//!              /     \
//!       is_odd?      is_even?         (split on parity)
//!            |          |
//!      "{i} is odd" "{i} is even"     (format each branch)
//!              \     /
//!               merge                 (recombine — at most one fires/tick)
//!                 |
//!               print
//! ```
//!
//! ```sh
//! cargo run -p wingfoil-next --release --example dual_mode
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: u32 = 10;
const PERIOD: Duration = Duration::from_millis(10);

// One definition — a valid fluent wiring function whose DAG is a
// split/recombine. The macro parses it to derive the DAG and expands to a
// module: `odds_evens::wire` (this function, verbatim),
// `odds_evens::interpreted()` (built through wire), and
// `odds_evens::compiled()` (the monomorphized schedule derived from the same
// tokens).
//
// `count` is referenced three times, so it is a *shared* apex node: the
// interpreted engine runs it once per cycle and fans the tick out, and the
// compiled engine emits it once and feeds every reader from the same slot.
// `merge` is the recombine — since a number is either odd or even, at most
// one branch fires on any tick.
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
    let run_for = RunFor::Cycles(CYCLES);

    // Interpreted: build the graph, run it, read the accumulated labels.
    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);

    // Compiled: the same tokens, monomorphized into a standalone runner.
    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for).unwrap();

    assert_eq!(interpreted, compiled, "engines must agree");

    for line in &interpreted {
        println!("{line}");
    }
    println!(
        "\n{} labels over {CYCLES} cycles — interpreted and compiled engines agree.",
        interpreted.len()
    );
}
