//! The `graph!` macro closes the dual-mode loop: its input is a single
//! **valid fluent wiring function**, which the macro parses to derive the
//! DAG. The same tokens expand to `wire()` (the function verbatim),
//! `interpreted()` (built through `wire`), and `compiled()` (fully
//! monomorphized) — so the engines cannot drift. These tests assert the two
//! expansions agree, and match the values the hand-written engines produced.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(10);

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

#[test]
fn macro_interpreted_matches_macro_compiled() {
    let run_for = RunFor::Cycles(12);

    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert_eq!(12, interpreted.len());
    assert_eq!("1 is odd", interpreted[0]);
    assert_eq!("2 is even", interpreted[1]);

    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}

#[test]
fn macro_engines_agree_on_duration_bound() {
    let run_for = RunFor::Duration(Duration::from_millis(55));
    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert!(!interpreted.is_empty());
    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}

/// The wiring function itself is exported verbatim as `wire` — usable as
/// ordinary fluent code, composed with manual wiring.
#[test]
fn macro_exports_the_wiring_fn_verbatim() {
    let g = GraphBuilder::new();
    let acc = odds_evens::wire(&g);
    let extra = acc.map(|v| v.len() as u64);
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(3, runner.value(&extra));
}

wingfoil_next::graph! {
    fn delayed_counts(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g
            .ticker(Duration::from_nanos(10))
            .count()
            .delay(Duration::from_nanos(100))
            .accumulate();
        acc
    }
}

/// Delay — state + self-scheduling — through the macro (as one chain with
/// anonymous intermediates), matching the classic engine's
/// `long_delay_works` timing on both expansions.
#[test]
fn macro_handles_delay_on_both_engines() {
    let run_for = RunFor::Duration(Duration::from_nanos(120));

    let (mut runner, acc) = delayed_counts::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert_eq!(vec![1, 2, 3, 4], interpreted);

    let (compiled,) = delayed_counts::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}

wingfoil_next::graph! {
    fn sampled(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let tick = g.ticker(Duration::from_nanos(100));
        let acc = g.constant(7u64).sample(&tick).accumulate();
        acc
    }
}

/// Sample's passive data edge + constant, both engines.
#[test]
fn macro_handles_sample_and_constant() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, acc) = sampled::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert_eq!(vec![7, 7, 7], interpreted);
    let (compiled,) = sampled::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}

wingfoil_next::graph! {
    fn configured(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let base = Duration::from_nanos(50);
        let period = base * 2;
        let threshold: u64 = 2;
        let count = g.ticker(period).count();
        let acc = count.map(move |i| i.saturating_sub(threshold)).accumulate();
        acc
    }
}

/// Arbitrary non-wiring statements in the body: config computed from locals,
/// and a closure capturing a local. Both engines run them identically.
#[test]
fn macro_allows_passthrough_statements() {
    let run_for = RunFor::Cycles(5);
    let (mut runner, acc) = configured::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert_eq!(vec![0, 0, 1, 2, 3], interpreted);

    let (compiled,) = configured::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}

wingfoil_next::graph! {
    fn staged(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let count = g.ticker(Duration::from_nanos(50)).count();
        let step: u64 = 3;
        let acc = count.map(move |i| i * step).accumulate();
        acc
    }
}

/// Passthrough statements may appear *between* wiring statements —
/// `compiled()` re-emits them interleaved in source order.
#[test]
fn macro_interleaves_passthrough_with_wiring() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, acc) = staged::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert_eq!(vec![3, 6, 9], interpreted);

    let (compiled,) = staged::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}

wingfoil_next::graph! {
    fn joined(g: &GraphBuilder) -> (Stream<Vec<u64>>, Stream<u64>) {
        let count = g.ticker(Duration::from_nanos(100)).count();
        let doubled = count.map(|i| i * 2);
        let acc = count.join(&doubled, |a, b| a + b).accumulate();
        (acc, doubled)
    }
}

/// Join + multiple outputs (tuple return), both engines.
#[test]
fn macro_handles_join_and_multiple_outputs() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, acc, doubled) = joined::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);
    assert_eq!(vec![3, 6, 9], interpreted);
    assert_eq!(6, runner.value(doubled));

    let (compiled_acc, compiled_doubled) = joined::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled_acc);
    assert_eq!(6, compiled_doubled);
}

wingfoil_next::graph! {
    fn stats(g: &GraphBuilder) -> (Stream<Vec<f64>>, Stream<Vec<f64>>) {
        let x = g.ticker(Duration::from_nanos(10)).count().map(|i| *i as f64);
        let smoothed = x.ewma_per_tick(0.5).accumulate();
        let windowed = x.rolling_mean(3).accumulate();
        (smoothed, windowed)
    }
}

/// Statistics ops (EWMA + rolling window) routed through the macro — a
/// non-closure `Cfg` and Default-seeded state — agree across both engines.
#[test]
fn macro_handles_statistics_on_both_engines() {
    let run_for = RunFor::Cycles(5);
    let (mut runner, smoothed, windowed) = stats::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let (i_smoothed, i_windowed) = (runner.value(smoothed), runner.value(windowed));
    // Samples are 1,2,3,4,5. EWMA(0.5) seeds on the first sample.
    assert_eq!(vec![1.0, 1.5, 2.25, 3.125, 4.0625], i_smoothed);
    // rolling_mean over a 3-wide window: 1, 1.5, 2, 3, 4.
    assert_eq!(vec![1.0, 1.5, 2.0, 3.0, 4.0], i_windowed);

    let (c_smoothed, c_windowed) = stats::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(i_smoothed, c_smoothed);
    assert_eq!(i_windowed, c_windowed);
}
