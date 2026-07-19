//! The dual-mode thesis, completed by the `graph!` macro: **one** wiring
//! definition expands to both an interpreted runner and a fully
//! monomorphized compiled runner. The two cannot drift — same tokens, same
//! `Op` semantics — and the compiled one gets the compiler's full
//! optimization across node boundaries.
//!
//! ```sh
//! cargo run -p wingfoil-next --release --example dual_mode
//! ```

use std::time::{Duration, Instant};

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: u32 = 200_000;
const PERIOD: Duration = Duration::from_micros(1);

// One definition, in ordinary fluent style — the macro parses these chains
// to derive the DAG. `evens_sum::interpreted()` re-emits the statements
// verbatim; `evens_sum::compiled()` is the monomorphized schedule derived
// from the same tokens.
wingfoil_next::graph! {
    mod evens_sum;
    out sum: u64;
    let count = g.ticker(PERIOD).count();
    let is_even = count.map(|i| i.is_multiple_of(2));
    let sum = count.filter(&is_even).fold(0u64, |acc, v| *acc += v);
}

fn main() {
    let run_for = RunFor::Cycles(CYCLES);

    let t = Instant::now();
    let (mut runner, sum) = evens_sum::interpreted();
    runner.run(HISTORICAL, run_for);
    let a: u64 = runner.value(sum);
    let interp_time = t.elapsed();

    let t = Instant::now();
    let (b,) = evens_sum::compiled(HISTORICAL, run_for);
    let compiled_time = t.elapsed();

    assert_eq!(a, b, "engines must agree");
    println!("sum of even counts over {CYCLES} cycles: {a} (both engines agree)");
    println!(
        "interpreted: {interp_time:?}  ({:.1} ns/cycle)",
        interp_time.as_nanos() as f64 / CYCLES as f64
    );
    println!(
        "compiled:    {compiled_time:?}  ({:.1} ns/cycle)",
        compiled_time.as_nanos() as f64 / CYCLES as f64
    );
    println!(
        "speedup:     {:.1}x  (run with --release for representative numbers)",
        interp_time.as_secs_f64() / compiled_time.as_secs_f64()
    );
}
