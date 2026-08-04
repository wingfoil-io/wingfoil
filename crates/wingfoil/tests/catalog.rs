//! Phase 2 node-catalog parity: each ported op reproduces the legacy
//! engine's observable behaviour for the equivalent graph. These mirror the
//! legacy nodes' own unit tests (`distinct`, `drop_small_change`,
//! `difference`, `limit`, `map_filter`) — same values, same tick suppression.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// `distinct` emits the first value then only on change — mirrors legacy
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

/// `drop_small_change` always propagates the first value, whatever the
/// predicate says — mirrors legacy
/// `drop_small_change::first_tick_always_propagates`.
#[test]
fn drop_small_change_first_tick_always_propagates() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.drop_small_change(|_, _| true).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1], r.value(&acc));
}

/// The predicate compares against the **last emitted** value, not the last
/// seen one, so an accumulating drift of individually-small steps ticks once
/// it crosses the threshold — mirrors legacy
/// `drop_small_change::compares_f64_changes_to_last_emitted_value`.
#[test]
fn drop_small_change_compares_to_last_emitted_value() {
    let g = GraphBuilder::new();
    let prices = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|count| match count {
            1 => 100.000_f64,
            2 => 100.005,
            3 => 100.020,
            _ => 100.025,
        });
    let acc = prices
        .drop_small_change(|current: &f64, previous: &f64| (current - previous).abs() < 0.01)
        // Tick *times* are part of the contract: the suppressed ticks must be
        // absent, not merely repeated values.
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(
        vec![(NanoTime::new(0), 100.000), (NanoTime::new(200), 100.020),],
        r.value(&acc)
    );
}

/// A predicate that never calls a change small passes every tick through —
/// mirrors legacy
/// `drop_small_change::propagates_every_tick_when_predicate_returns_false`.
#[test]
fn drop_small_change_propagates_when_predicate_is_false() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.drop_small_change(|_, _| false).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![1, 2, 3, 4], r.value(&acc));
}

/// An equality predicate makes `drop_small_change` degenerate to `distinct`,
/// including the first-value-equal-to-default case.
#[test]
fn drop_small_change_with_equality_matches_distinct() {
    let g = GraphBuilder::new();
    let bucketed = g
        .ticker(Duration::from_nanos(10))
        .count()
        .map(|i| (i - 1) / 3); // 0,0,0,1,1,1,2,2,2
    let dropped = bucketed
        .drop_small_change(|current: &u64, previous: &u64| current == previous)
        .accumulate();
    let distinct = bucketed.distinct().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(vec![0, 1, 2], r.value(&dropped));
    assert_eq!(r.value(&distinct), r.value(&dropped));
}

/// `difference` is quiet on the first tick, then emits deltas — mirrors
/// legacy `difference::{first_tick_does_not_emit, delta_is_correct}`.
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

/// `limit` passes the first N then suppresses — mirrors legacy
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

/// A passive `join` input is read but does not trigger — mirrors legacy
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

/// `map_filter` maps and filters in one pass — mirrors legacy
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

/// `throttle` suppresses ticks that arrive within `interval` of the last
/// emit — mirrors legacy `throttle::throttle_suppresses_fast_ticks`
/// (source every 10ns, interval 25ns → emit at 0, 30, 60, ...).
#[test]
fn throttle_suppresses_fast_ticks() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // ticks at 0,10,20,30,...
    let acc = count.throttle(Duration::from_nanos(25)).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(65)))
        .unwrap();
    // t=0 emit(1); 10,20 suppressed; t=30 emit(4); 40,50 suppressed; t=60 emit(7).
    assert_eq!(vec![1, 4, 7], r.value(&acc));
}

/// `window` buffers values and flushes them on each time boundary — mirrors
/// legacy `window::window_stream_works` (100/250 grouping: [1,2,3], [4,5],
/// [6,7,8], …). Exercises the `Ctx::is_last_cycle` engine service and a
/// `start` hook that sets the first boundary.
#[test]
fn window_flushes_on_time_boundaries() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.window(Duration::from_nanos(250)).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(
        vec![vec![1, 2, 3], vec![4, 5], vec![6, 7, 8]],
        r.value(&acc)
    );
}

/// A zero-length `window` must still terminate. `cycle` walks the boundary
/// forward with `while next_window <= now { next_window += interval }`, which
/// never ends for a zero interval — `window(Duration::ZERO)` hung the run
/// outright, rather than degenerating like every other size-configured op in
/// the catalog (the rolling ops all clamp with `(*cfg).max(1)`). Floored at the
/// engine clock's one-nanosecond resolution, every cycle is its own boundary,
/// so each value flushes alone.
#[test]
fn zero_length_window_flushes_every_cycle_rather_than_hanging() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.window(Duration::ZERO).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // The first cycle has nothing buffered yet (the boundary is one tick ahead
    // of it), so it stays quiet; each later cycle then flushes the single value
    // its predecessor buffered. The 4th cycle's own value is buffered behind a
    // boundary flush that already fired, which is `window`'s ordinary
    // last-cycle behaviour at any interval — the extra flush is skipped when the
    // boundary already emitted.
    assert_eq!(vec![vec![1], vec![2], vec![3]], r.value(&acc));
}

/// `buffer` flushes a `Vec` every `capacity` values, plus a final partial
/// flush — mirrors legacy `buffer::buffer_stream_works`.
#[test]
fn buffer_flushes_by_capacity() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // 1..=7
    let acc = count.buffer(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(7)).unwrap();
    // [1,2,3], [4,5,6], then the final partial [7] on the last cycle.
    assert_eq!(vec![vec![1, 2, 3], vec![4, 5, 6], vec![7]], r.value(&acc));
}

/// `join3` (trimap) combines three streams — mirrors legacy
/// `trimap::trimap_all_active`.
#[test]
fn join3_combines_three_streams() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let doubled = count.map(|i| i * 2);
    let tripled = count.map(|i| i * 3);
    let acc = count
        .join3(&doubled, &tripled, |a, b, c| a + b + c)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // n + 2n + 3n = 6n for n = 1, 2, 3.
    assert_eq!(vec![6, 12, 18], r.value(&acc));
}

/// `with_time` pairs each value with the engine time — mirrors legacy
/// `with_time::timestamps_match_graph_time`.
#[test]
fn with_time_pairs_value_with_engine_time() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    let items = r.value(&acc);
    let expected = vec![
        (NanoTime::new(0), 1),
        (NanoTime::new(100), 2),
        (NanoTime::new(200), 3),
        (NanoTime::new(300), 4),
    ];
    assert_eq!(expected, items);
}

/// `ticked_at` emits the engine time on each tick — mirrors legacy
/// `graph_state::ticked_at_emits_graph_time`.
#[test]
fn ticked_at_emits_engine_time() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.ticked_at().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)],
        r.value(&acc)
    );
}

/// `not` negates a bool stream — mirrors legacy `not_inverts_bool_stream`.
#[test]
fn not_inverts_bool_stream() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let is_even = count.map(|i| i.is_multiple_of(2)); // false,true,false
    let acc = is_even.not().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![true, false, true], r.value(&acc));
}

/// `inspect` observes each value and passes it through unchanged — mirrors
/// legacy `inspect::inspect_observes_and_passes_through`.
#[test]
fn inspect_observes_and_passes_through() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let tap = seen.clone();
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let acc = count
        .inspect(move |v| tap.borrow_mut().push(*v))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // Observed and passed through identically.
    assert_eq!(vec![1, 2, 3], *seen.borrow());
    assert_eq!(vec![1, 2, 3], r.value(&acc));
}

/// `ticked_at_elapsed` emits elapsed engine time (`now - start`) on each tick.
/// Tested from a **non-zero** start so it is distinguishable from `ticked_at`.
#[test]
fn ticked_at_elapsed_emits_elapsed_time() {
    let start = NanoTime::new(1000);
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.ticked_at_elapsed().accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(start), RunFor::Cycles(3))
        .unwrap();
    // Ticks at 1000, 1100, 1200; elapsed = 0, 100, 200.
    assert_eq!(
        vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)],
        r.value(&acc)
    );
}

/// `window` from a **non-zero** start. The boundary is anchored at `ctx.time()`
/// during `start` (= ZERO, a quirk shared bug-for-bug with legacy — see the
/// fable review), so the 250ns boundaries fall on 250/500/750/… absolute time,
/// giving the same grouping as a zero-start run even though the data lives at
/// 1000+. This pins that behaviour so a unilateral "fix" can't drift silently.
#[test]
fn window_from_non_zero_start_anchors_boundaries_at_zero() {
    let start = NanoTime::new(1000);
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.window(Duration::from_nanos(250)).accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(start), RunFor::Cycles(9))
        .unwrap();
    assert_eq!(
        vec![vec![1, 2, 3], vec![4, 5], vec![6, 7, 8]],
        r.value(&acc)
    );
}
