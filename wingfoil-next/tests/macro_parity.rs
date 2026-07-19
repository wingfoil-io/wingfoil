//! The `graph!` macro closes the dual-mode loop: **one** wiring definition
//! expands to both `interpreted()` and `compiled()`, so the two engines
//! cannot drift — no fingerprints, no re-supplied closures, no hand-kept
//! expansions. These tests assert the two expansions agree, and match the
//! values the hand-written engines produced.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(10);

wingfoil_next::graph! {
    mod odds_evens;
    out acc: Vec<String>;
    tick = ticker(PERIOD);
    count = fold(tick, 0u64, |acc, _| *acc += 1);
    is_even = map(count, |i| i.is_multiple_of(2));
    is_odd = map(is_even, |b| !b);
    odds = filter(count, is_odd);
    odd_str = map(odds, |i| format!("{i} is odd"));
    evens = filter(count, is_even);
    even_str = map(evens, |i| format!("{i} is even"));
    merged = merge(odd_str, even_str);
    acc = fold(merged, Vec::new(), |acc, v: &String| acc.push(v.clone()));
}

#[test]
fn macro_interpreted_matches_macro_compiled() {
    let run_for = RunFor::Cycles(12);

    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for);
    let interpreted = runner.value(acc);
    assert_eq!(12, interpreted.len());
    assert_eq!("1 is odd", interpreted[0]);
    assert_eq!("2 is even", interpreted[1]);

    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for);
    assert_eq!(interpreted, compiled);
}

#[test]
fn macro_engines_agree_on_duration_bound() {
    let run_for = RunFor::Duration(Duration::from_millis(55));
    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for);
    let interpreted = runner.value(acc);
    assert!(!interpreted.is_empty());
    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for);
    assert_eq!(interpreted, compiled);
}

wingfoil_next::graph! {
    mod delayed_counts;
    out acc: Vec<u64>;
    tick = ticker(Duration::from_nanos(10));
    count = fold(tick, 0u64, |acc, _| *acc += 1);
    delayed = delay(count, Duration::from_nanos(100));
    acc = fold(delayed, Vec::new(), |acc, v: &u64| acc.push(*v));
}

/// Delay — state + self-scheduling — through the macro, matching the classic
/// engine's `long_delay_works` timing on both expansions.
#[test]
fn macro_handles_delay_on_both_engines() {
    let run_for = RunFor::Duration(Duration::from_nanos(120));

    let (mut runner, acc) = delayed_counts::interpreted();
    runner.run(HISTORICAL, run_for);
    let interpreted = runner.value(acc);
    assert_eq!(vec![1, 2, 3, 4], interpreted);

    let (compiled,) = delayed_counts::compiled(HISTORICAL, run_for);
    assert_eq!(interpreted, compiled);
}

wingfoil_next::graph! {
    mod sampled;
    out acc: Vec<u64>;
    tick = ticker(Duration::from_nanos(100));
    value = constant(7u64);
    sampled_value = sample(value, tick);
    acc = fold(sampled_value, Vec::new(), |acc, v: &u64| acc.push(*v));
}

/// Sample's passive data edge + join-free constant, both engines.
#[test]
fn macro_handles_sample_and_constant() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, acc) = sampled::interpreted();
    runner.run(HISTORICAL, run_for);
    let interpreted = runner.value(acc);
    assert_eq!(vec![7, 7, 7], interpreted);
    let (compiled,) = sampled::compiled(HISTORICAL, run_for);
    assert_eq!(interpreted, compiled);
}
