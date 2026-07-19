//! Phase 2 node-catalog parity: each ported op reproduces the classic
//! engine's observable behaviour for the equivalent graph. These mirror the
//! classic nodes' own unit tests (`distinct`, `difference`, `limit`,
//! `map_filter`) — same values, same tick suppression.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// `distinct` emits the first value then only on change — mirrors classic
/// `distinct::suppresses_repeated_values`.
#[test]
fn distinct_suppresses_repeats() {
    let g = GraphBuilder::new();
    // count/3 rounded: 1,1,1,2,2,2,3,3,3 → distinct → 0,1,1,2,2,3 ... use a
    // deterministic repeating pattern via map.
    let count = g.ticker(Duration::from_nanos(10)).count();
    let bucketed = count.map(|i| (i - 1) / 3); // 0,0,0,1,1,1,2,2,2
    let acc = bucketed.distinct().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(vec![0, 1, 2], r.value(&acc));
}

/// `distinct` still emits a genuine first value equal to the default (0).
#[test]
fn distinct_emits_first_value_equal_to_default() {
    let g = GraphBuilder::new();
    let zero = g.ticker(Duration::from_nanos(10)).count().map(|_| 0u64);
    let acc = zero.distinct().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // 0,0,0 → distinct emits the first 0 only.
    assert_eq!(vec![0], r.value(&acc));
}

/// `difference` is quiet on the first tick, then emits deltas — mirrors
/// classic `difference::{first_tick_does_not_emit, delta_is_correct}`.
#[test]
fn difference_emits_deltas_after_first() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // 1,2,3,4
    let acc = count.difference().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // deltas of 1,2,3,4 → 1,1,1 (first tick suppressed).
    assert_eq!(vec![1, 1, 1], r.value(&acc));
}

/// `limit` passes the first N then suppresses — mirrors classic
/// `limit::suppresses_after_limit_reached`.
#[test]
fn limit_caps_ticks() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let acc = count.limit(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(10)).unwrap();
    assert_eq!(vec![1, 2, 3], r.value(&acc));
}

/// A passive `join` input is read but does not trigger — mirrors classic
/// `bimap::bimap_passive_does_not_trigger`. The combine fires only when the
/// active (slow) input ticks, reading the passive (fast) input's current
/// value at that instant.
#[test]
fn join_passive_reads_without_triggering() {
    let g = GraphBuilder::new();
    let slow = g.ticker(Duration::from_nanos(100)).count(); // 1,2,3 at 0,100,200
    let fast = g.ticker(Duration::from_nanos(10)).count(); // ticks 10x as often
    let combined = slow.join_passive(&fast, |s, f| (*s, *f));
    let acc = combined.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(205)))
        .unwrap();
    // Fires only on the slow ticks (3 of them), reading fast's live value:
    // t=0 fast=1, t=100 fast=11, t=200 fast=21.
    assert_eq!(vec![(1, 1), (2, 11), (3, 21)], r.value(&acc));
}

/// `map_filter` maps and filters in one pass — mirrors classic
/// `map_filter::emits_when_function_returns_true` (odd inputs squared).
#[test]
fn map_filter_maps_and_filters() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // 1..=6
    let acc = count
        .map_filter(|i| (i * i, i % 2 == 1)) // squares of odds: 1, 9, 25
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![1, 9, 25], r.value(&acc));
}
