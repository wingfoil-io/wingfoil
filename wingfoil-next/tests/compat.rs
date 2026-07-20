//! Phase 6 facade: classic-style wingfoil programs run on the new engine.
//! These are written exactly as classic code is — free source functions,
//! `stream.run(...)`, `stream.peek_value()` — but every stream is backed by
//! the new `Op`/`Builder` engine. This is the compatibility surface that
//! lets existing code (and the Python bindings) migrate unchanged.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::compat::{constant, ticker};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// The canonical classic snippet: count a ticker and read the result.
#[test]
fn classic_counter_runs_on_the_new_engine() {
    let counted = ticker(Duration::from_nanos(100)).count();
    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());
}

/// A classic chain: ticker → count → map → filter → accumulate, run and read
/// off the accumulator, all in the classic idiom.
#[test]
fn classic_chain_maps_filters_accumulates() {
    let count = ticker(Duration::from_nanos(100)).count();
    let is_even = count.map(|i| i.is_multiple_of(2));
    let evens = count.filter(&is_even).accumulate();
    evens.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![2, 4, 6], evens.peek_value());
}

/// A classic fold (running sum) driven off a counter.
#[test]
fn classic_fold_sums() {
    let total = ticker(Duration::from_nanos(100))
        .count()
        .fold(0u64, |acc, v| *acc += v);
    total.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // 1 + 2 + 3 + 4
    assert_eq!(10, total.peek_value());
}

/// Classic `constant` + `delay`, matching the classic engine's timing.
#[test]
fn classic_constant_and_delay() {
    let delayed = constant(7u64).delay(Duration::from_nanos(50)).accumulate();
    delayed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // constant ticks once at t=0; delayed re-emits it at t=50.
    assert_eq!(vec![7], delayed.peek_value());
}

/// Peeking before the graph has run is a reachable user error. `peek_value`
/// mirrors the classic infallible signature (`-> T`), so it enforces the
/// precondition with an explanatory panic rather than an out-of-bounds one.
#[test]
#[should_panic(expected = "Signal::run must be called before Signal::peek_value")]
fn peek_before_run_panics_with_a_clear_message() {
    let counted = ticker(Duration::from_nanos(100)).count();
    // No `run` — this must panic with the documented precondition message,
    // never a bare index-out-of-bounds.
    let _ = counted.peek_value();
}

/// Re-running is deferred: the builder is consumed by the first run, so a
/// second `run` must surface a clear error instead of silently running an
/// empty graph and then panicking out-of-bounds in `peek_value`. The first
/// run's result stays readable afterwards.
#[test]
fn second_run_errors_and_leaves_first_result_intact() {
    let counted = ticker(Duration::from_nanos(100)).count();
    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());

    let err = counted
        .run(HISTORICAL, RunFor::Cycles(5))
        .expect_err("a second run must error, not silently run an empty graph");
    assert!(
        err.to_string().contains("called more than once"),
        "unexpected error message: {err}"
    );

    // The first run's runner is untouched, so peek still returns its value.
    assert_eq!(5, counted.peek_value());
}
