//! Phase 2 node-catalog parity for the remaining scheduling / structural
//! nodes: `never`, `delay_with_reset`, the node-level flow ops (`node_flow`'s
//! throttle / delay / limit / filter / feedback, run here over the unit-stream
//! path), and the `combine` / `split` / `collapse` structural ops. Each test
//! mirrors the classic node's own unit test — reproducing the same values and
//! the same tick times.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

// --- never -----------------------------------------------------------------

/// `never` never ticks, so nothing downstream of it ever fires — mirrors
/// classic `never::never_does_not_trigger_downstream`.
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

// --- delay_with_reset (classic `delay_with_reset`) -------------------------

/// With `never()` as the reset trigger, `delay_with_reset` degrades to a plain
/// `delay` — mirrors classic `delay_with_reset::delay_never_reset`.
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
/// classic `delay_with_reset::delay_with_reset_snaps_on_trigger` (trigger every
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
/// land on the same instant — mirrors classic
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
/// trigger — mirrors classic
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
/// mirrors classic `node_flow::node_throttle_suppresses_fast_ticks` (10ns
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

/// A zero interval throttles nothing — mirrors classic
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

/// Delaying a source shifts its ticks by the interval — mirrors classic
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

/// A zero delay passes ticks through immediately — mirrors classic
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

/// `limit` caps the number of ticks passed — mirrors classic
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

/// `filter` gates ticks by a condition stream — mirrors classic
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

/// Feedback on a unit stream re-enters one cycle (`+1`) later — mirrors classic
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
/// order — mirrors classic `combine::combine_works`.
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
/// classic `mod::split_decomposes_tuple_stream`.
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

/// `collapse` emits the last item of a non-empty iterator value and stays
/// quiet on an empty one — mirrors classic `mod::collapse_skips_empty_iterator`.
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
