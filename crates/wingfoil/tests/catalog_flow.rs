//! Phase 2 node-catalog parity for the remaining scheduling / structural
//! nodes: `never`, `delay_with_reset`, the node-level flow ops (`node_flow`'s
//! throttle / delay / limit / filter / feedback, run here over the unit-stream
//! path), and the `combine` / `split` / `collapse` structural ops. Each test
//! mirrors the legacy node's own unit test — reproducing the same values and
//! the same tick times.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

// --- never -----------------------------------------------------------------

/// `never` never ticks, so nothing downstream of it ever fires — mirrors
/// legacy `never::never_does_not_trigger_downstream`.
#[test]
fn never_never_triggers_downstream() {
    let counter = Rc::new(RefCell::new(0u32));
    let c2 = counter.clone();
    let g = GraphBuilder::new();
    // A ticker drives the clock forward; `never`'s branch must stay silent.
    let _clock = g.ticker(Duration::from_nanos(10)).count();
    let _sink = g.never().for_each(move |_| {
        *c2.borrow_mut() += 1;
        Ok(())
    });
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(0, *counter.borrow());
}

// --- delay_with_reset (legacy `delay_with_reset`) -------------------------

/// With `never()` as the reset trigger, `delay_with_reset` degrades to a plain
/// `delay` — mirrors legacy `delay_with_reset::delay_never_reset`.
#[test]
fn delay_with_reset_never_reset_matches_delay() {
    let period = Duration::from_nanos(100);
    let g = GraphBuilder::new();
    let src = g.ticker(period).count();
    let never = g.never();
    let with_reset = src
        .delay_with_reset(period * 3, &never)
        .with_time()
        .accumulate();
    let plain = src.delay(period * 3).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(20)).unwrap();
    assert_eq!(r.value(&with_reset), r.value(&plain));
}

fn assert_snaps_on_trigger(trigger_period: Duration, expected: Vec<(u64, u64, u64)>) {
    let period = Duration::from_nanos(100);
    let g = GraphBuilder::new();
    let source = g.ticker(period).count();
    let trigger = g.ticker(trigger_period);
    let delayed = source.delay(period * 5);
    let reset = source.delay_with_reset(period * 5, &trigger);
    let acc = source
        .join3(&delayed, &reset, |a, b, c| (*a, *b, *c))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(period * 20)).unwrap();
    assert_eq!(expected, r.value(&acc));
}

/// The reset trigger snaps the delayed output back to the live value — mirrors
/// legacy `delay_with_reset::delay_with_reset_snaps_on_trigger` (trigger every
/// 1000ns).
#[test]
fn delay_with_reset_snaps_on_trigger() {
    assert_snaps_on_trigger(
        Duration::from_nanos(1000),
        vec![
            (1, 1, 1),
            (2, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
            (5, 1, 1),
            (6, 1, 1),
            (7, 2, 2),
            (8, 3, 3),
            (9, 4, 4),
            (10, 5, 5),
            (11, 6, 11),
            (12, 7, 11),
            (13, 8, 11),
            (14, 9, 11),
            (15, 10, 11),
            (16, 11, 11),
            (17, 12, 12),
            (18, 13, 13),
            (19, 14, 14),
            (20, 15, 15),
            (21, 16, 21),
            (22, 17, 21),
        ],
    );
}

/// A second reset cadence (every 750ns), where a trigger and a delayed pop can
/// land on the same instant — mirrors legacy
/// `delay_with_reset::delay_with_reset_snaps_on_trigger_2`.
#[test]
fn delay_with_reset_snaps_on_trigger_2() {
    assert_snaps_on_trigger(
        Duration::from_nanos(750),
        vec![
            (1, 1, 1),
            (2, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
            (5, 1, 1),
            (6, 1, 1),
            (7, 2, 2),
            (8, 3, 3),
            (8, 3, 8),
            (9, 4, 8),
            (10, 5, 8),
            (11, 6, 8),
            (12, 7, 8),
            (13, 8, 8),
            (14, 9, 9),
            (15, 10, 10),
            (16, 11, 16),
            (17, 12, 16),
            (18, 13, 16),
            (19, 14, 16),
            (20, 15, 16),
            (21, 16, 16),
            (22, 17, 17),
        ],
    );
}

/// Zero delay passes every value through immediately, regardless of the reset
/// trigger — mirrors legacy
/// `delay_with_reset::delay_with_reset_zero_delay_passes_through_immediately`.
#[test]
fn delay_with_reset_zero_delay_passes_through_immediately() {
    let period = Duration::from_nanos(100);
    let g = GraphBuilder::new();
    let src = g.ticker(period).count();
    let never = g.never();
    let acc = src
        .delay_with_reset(Duration::ZERO, &never)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // Zero delay: every tick passes through immediately, unchanged.
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(100), 2),
            (NanoTime::new(200), 3),
        ],
        r.value(&acc)
    );
}

// --- node_flow (node-level flow ops over the unit-stream path) --------------

/// Throttling a fast source suppresses ticks closer than the interval —
/// mirrors legacy `node_flow::node_throttle_suppresses_fast_ticks` (10ns
/// source, 25ns interval → t = 0, 30, 60).
#[test]
fn node_throttle_suppresses_fast_ticks() {
    let g = GraphBuilder::new();
    let acc = g
        .ticker(Duration::from_nanos(10))
        .throttle(Duration::from_nanos(25))
        .count()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(60)))
        .unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(30), 2),
            (NanoTime::new(60), 3),
        ],
        r.value(&acc)
    );
}

/// A zero interval throttles nothing — mirrors legacy
/// `node_flow::node_throttle_zero_interval_passes_all`.
#[test]
fn node_throttle_zero_interval_passes_all() {
    let g = GraphBuilder::new();
    let acc = g
        .ticker(Duration::from_nanos(10))
        .throttle(Duration::from_nanos(0))
        .count()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(10), 2),
            (NanoTime::new(20), 3),
        ],
        r.value(&acc)
    );
}

/// `audit` keeps the first deadline fixed while replacing the pending value.
/// A 10ns source with a 25ns window therefore emits values 3 and 6 at 25ns
/// and 55ns even though the source never goes quiet.
#[test]
fn audit_emits_latest_value_on_each_fixed_window() {
    let g = GraphBuilder::new();
    let audited = g
        .ticker(Duration::from_nanos(10))
        .count()
        .audit(Duration::from_nanos(25))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    assert_eq!(
        vec![(NanoTime::new(25), 3), (NanoTime::new(55), 6)],
        r.value(&audited)
    );
}

#[test]
fn audit_starts_a_new_window_when_input_hits_the_deadline() {
    let g = GraphBuilder::new();
    let audited = g
        .ticker(Duration::from_nanos(10))
        .count()
        .audit(Duration::from_nanos(20))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(7)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(20), 2),
            (NanoTime::new(40), 4),
            // The t=60 deadline collides with the final source value. The
            // final flush wins so the newest value is not dropped.
            (NanoTime::new(60), 7),
        ],
        r.value(&audited)
    );
}

/// A lone value remains pending until the fixed window elapses.
#[test]
fn audit_lone_value_emits_after_the_window() {
    let g = GraphBuilder::new();
    let audited = g
        .ticker(Duration::from_nanos(100))
        .count()
        .limit(1)
        .audit(Duration::from_nanos(25))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();
    assert_eq!(vec![(NanoTime::new(25), 1)], r.value(&audited));
}

/// A value still pending on the final cycle is flushed at that cycle's time
/// instead of being dropped when the run ends.
#[test]
fn audit_flushes_pending_value_on_the_last_cycle() {
    let g = GraphBuilder::new();
    let audited = g
        .ticker(Duration::from_nanos(100))
        .count()
        .audit(Duration::from_nanos(25))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    assert_eq!(vec![(NanoTime::ZERO, 1)], r.value(&audited));
}

/// A zero window has no suppression interval and emits inline.
#[test]
fn audit_zero_window_passes_every_value_inline() {
    let g = GraphBuilder::new();
    let audited = g
        .ticker(Duration::from_nanos(10))
        .count()
        .audit(Duration::ZERO)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(10), 2),
            (NanoTime::new(20), 3),
        ],
        r.value(&audited)
    );
}

/// Every source tick moves the deadline. The stale wakes at t=25, 35 and 45
/// stay quiet; only the current t=55 deadline may emit the trailing value.
#[test]
fn debounce_rearms_and_ignores_stale_wakes() {
    let g = GraphBuilder::new();
    let debounced = g
        .ticker(Duration::from_nanos(10))
        .count()
        .limit(4)
        .debounce(Duration::from_nanos(25))
        .with_time()
        .accumulate();
    let mut r = g.build();
    // Run one cycle past the t=55 deadline so the value cannot come from the
    // final-cycle flush.
    r.run(HISTORICAL, RunFor::Cycles(11)).unwrap();
    assert_eq!(vec![(NanoTime::new(55), 4)], r.value(&debounced));
}

#[test]
fn debounce_lone_value_emits_after_the_quiet_period() {
    let g = GraphBuilder::new();
    let debounced = g
        .ticker(Duration::from_nanos(100))
        .count()
        .limit(1)
        .debounce(Duration::from_nanos(25))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();
    assert_eq!(vec![(NanoTime::new(25), 1)], r.value(&debounced));
}

/// A source tick at an old deadline re-arms from that instant instead of
/// emitting the superseded pending value.
#[test]
fn debounce_input_at_the_deadline_rearms_without_emitting() {
    let g = GraphBuilder::new();
    let debounced = g
        .ticker(Duration::from_nanos(10))
        .count()
        .limit(3)
        .debounce(Duration::from_nanos(20))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![(NanoTime::new(40), 3)], r.value(&debounced));
}

/// A continuously busy source never reaches its armed deadline. The only
/// output is the settled end-of-run flush on the final source cycle.
#[test]
fn debounce_busy_source_emits_only_the_final_flush() {
    let g = GraphBuilder::new();
    let debounced = g
        .ticker(Duration::from_nanos(10))
        .count()
        .debounce(Duration::from_nanos(25))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![(NanoTime::new(30), 4)], r.value(&debounced));
}

#[test]
fn debounce_zero_period_passes_every_value_inline() {
    let g = GraphBuilder::new();
    let debounced = g
        .ticker(Duration::from_nanos(10))
        .count()
        .debounce(Duration::ZERO)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(10), 2),
            (NanoTime::new(20), 3),
        ],
        r.value(&debounced)
    );
}

/// `start_with` emits its configured value at the declared run start when the
/// source has not produced yet, then hands over to the source without a gap.
#[test]
fn start_with_emits_at_start_then_hands_over_to_a_later_source() {
    let g = GraphBuilder::new();
    let values = g
        .constant(7u64)
        .delay(Duration::from_nanos(5))
        .start_with(1)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![(NanoTime::ZERO, 1), (NanoTime::new(5), 7)],
        r.value(&values)
    );
}

/// A real source value at `start_time` wins the tie, so the configured initial
/// value never hides or duplicates source data.
#[test]
fn start_with_prefers_a_source_tick_at_start_time() {
    let g = GraphBuilder::new();
    let values = g.constant(7u64).start_with(1).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    assert_eq!(vec![(NanoTime::ZERO, 7)], r.value(&values));
}

/// Delaying a source shifts its ticks by the interval — mirrors legacy
/// `node_flow::node_delay_shifts_ticks` (100ns source, 10ns delay → arrives at
/// t = 10, 110, 210).
#[test]
fn node_delay_shifts_ticks() {
    let g = GraphBuilder::new();
    let acc = g
        .ticker(Duration::from_nanos(100))
        .delay(Duration::from_nanos(10))
        .count()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(10), 1),
            (NanoTime::new(110), 2),
            (NanoTime::new(210), 3),
        ],
        r.value(&acc)
    );
}

/// A zero delay passes ticks through immediately — mirrors legacy
/// `node_flow::node_delay_zero_passes_immediately`.
#[test]
fn node_delay_zero_passes_immediately() {
    let g = GraphBuilder::new();
    let acc = g
        .ticker(Duration::from_nanos(10))
        .delay(Duration::from_nanos(0))
        .count()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(10), 2),
            (NanoTime::new(20), 3),
        ],
        r.value(&acc)
    );
}

/// `limit` caps the number of ticks passed — mirrors legacy
/// `node_flow::node_limit_caps_ticks` (limit 3).
#[test]
fn node_limit_caps_ticks() {
    let g = GraphBuilder::new();
    let acc = g
        .ticker(Duration::from_nanos(10))
        .limit(3)
        .count()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(90)))
        .unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(10), 2),
            (NanoTime::new(20), 3),
        ],
        r.value(&acc)
    );
}

/// `filter` gates ticks by a condition stream — mirrors legacy
/// `node_flow::node_filter_gates_ticks` (pass only even counts → t = 10, 30,
/// 50).
#[test]
fn node_filter_gates_ticks() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(10));
    let is_even = src.count().map(|i| i % 2 == 0);
    let acc = src.filter(&is_even).count().with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(50)))
        .unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(10), 1),
            (NanoTime::new(30), 2),
            (NanoTime::new(50), 3),
        ],
        r.value(&acc)
    );
}

/// Feedback on a unit stream re-enters one cycle (`+1`) later — mirrors legacy
/// `node_flow::node_feedback_sends_signal` (rx ticks at t = 1, 101, 201).
#[test]
fn node_feedback_sends_signal() {
    let period = Duration::from_nanos(100);
    let g = GraphBuilder::new();
    let (rx, sink) = g.feedback::<()>();
    // Fire feedback on every source tick.
    let _fb = g.ticker(period).feedback(&sink);
    let acc = rx.count().with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(period * 2)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(1), 1),
            (NanoTime::new(101), 2),
            (NanoTime::new(201), 3),
        ],
        r.value(&acc)
    );
}

// --- combine / split / collapse (structural) -------------------------------

/// `combine` gathers every same-instant value into one burst, in argument
/// order — mirrors legacy `combine::combine_works`.
#[test]
fn combine_gathers_same_instant_values() {
    fn burst_of(items: &[u64]) -> Burst<u64> {
        let mut b = Burst::new();
        for &i in items {
            b.push(i);
        }
        b
    }
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_micros(1)).count();
    let streams: Vec<_> = (0..3).map(|i| src.map(move |x| x * 10u64.pow(i))).collect();
    let acc = g.combine(&streams).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            burst_of(&[1, 10, 100]),
            burst_of(&[2, 20, 200]),
            burst_of(&[3, 30, 300]),
        ],
        r.value(&acc)
    );
}

/// `split` decomposes a stream of pairs into its two components — mirrors
/// legacy `mod::split_decomposes_tuple_stream`.
#[test]
fn split_decomposes_tuple_stream() {
    let g = GraphBuilder::new();
    let pairs = g
        .ticker(Duration::from_nanos(10))
        .count()
        .map(|i| (i * 10, i * 20));
    let (a, b) = pairs.split();
    let aa = a.accumulate();
    let bb = b.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    assert_eq!(vec![10u64], r.value(&aa));
    assert_eq!(vec![20u64], r.value(&bb));
}

/// `filter_none` drops `None`, passing through just the `Some` payloads and
/// leaving the downstream quiet on the cycles that produced nothing — the
/// fluent counterpart of `tests/signal.rs::legacy_filter_none_drops_none`.
#[test]
fn filter_none_drops_none() {
    let g = GraphBuilder::new();
    // 1 → None, 2 → Some(20), 3 → None, 4 → Some(40).
    let opts = g.ticker(Duration::from_nanos(10)).count().map(|i| {
        if i.is_multiple_of(2) {
            Some(i * 10)
        } else {
            None
        }
    });
    let acc = opts.filter_none().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![20u64, 40], r.value(&acc));
}

/// `filter_map` emits the `Some` payloads and stays quiet on `None`, asserting
/// tick *times* as well as values so the suppressed cycles are pinned: a 10ns
/// ticker counts 1..=4 at t = 0/10/20/30, only the odd counts survive, and the
/// two even instants being **absent** from the accumulation is the filtering
/// half of the contract.
#[test]
fn filter_map_emits_some_and_stays_quiet_on_none() {
    let g = GraphBuilder::new();
    let counts = g.ticker(Duration::from_nanos(10)).count();
    let acc = counts
        .filter_map(|i: &u64| if i % 2 == 1 { Some(i * i) } else { None })
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(
        vec![(NanoTime::new(0), 1u64), (NanoTime::new(20), 9)],
        r.value(&acc)
    );
}

/// A `filter_map` that never emits leaves the downstream completely untouched:
/// the sink is wired and the source ticks four times, but no tick reaches past
/// the node.
#[test]
fn filter_map_all_none_never_ticks_downstream() {
    let seen = Rc::new(RefCell::new(0u32));
    let sink_hits = seen.clone();
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_nanos(10))
        .count()
        .filter_map(|_: &u64| None::<u64>)
        .for_each(move |_| {
            *sink_hits.borrow_mut() += 1;
            Ok(())
        });
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(0, *seen.borrow());
}

/// `filter_map` is exactly `map_filter` with the emit decision spelled as an
/// `Option` — same values, same tick times, wired side by side in one graph.
#[test]
fn filter_map_matches_map_filter() {
    let g = GraphBuilder::new();
    let counts = g.ticker(Duration::from_nanos(10)).count();
    let sugar = counts
        .filter_map(|i: &u64| if *i > 2 { Some(i * 10) } else { None })
        .with_time()
        .accumulate();
    let primitive = counts
        .map_filter(|i: &u64| if *i > 2 { (i * 10, true) } else { (0, false) })
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(20), 30u64),
            (NanoTime::new(30), 40),
            (NanoTime::new(40), 50),
        ],
        r.value(&sugar)
    );
    assert_eq!(r.value(&primitive), r.value(&sugar));
}

/// `collapse` emits the last item of a non-empty iterator value and stays
/// quiet on an empty one — mirrors legacy `mod::collapse_skips_empty_iterator`.
#[test]
fn collapse_skips_empty_iterator() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    // cycle 1 → [1, 2] (collapses to 2); cycle 2 → [] (collapse stays quiet).
    let vecs = count.map(|i| if *i == 1 { vec![1u64, 2] } else { Vec::new() });
    let acc = vecs.collapse::<u64>().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();
    assert_eq!(vec![2u64], r.value(&acc));
}
