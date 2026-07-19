//! Phase 0.2 spike: feedback edges — the one true DAG-breaker. The source
//! stream has no upstreams (so the graph stays acyclic); the sink pushes onto
//! a shared time-queue and schedules the source to emit one cycle later. This
//! reproduces the classic engine's `feedback_active_works` behaviour.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Mirrors `feedback_passive_works` (feedback.rs): a counter joined with a
/// *passively* read feedback value. The feedback input is read but does not
/// trigger, so the combine advances in step with the counter (once per
/// period) rather than on every feedback delivery — giving the digit-shift
/// progression 1, 12, 123, 1234, 12345, 123456.
#[test]
fn feedback_passive_matches_classic_engine() {
    let period = Duration::from_nanos(100);
    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();
    let source = g.ticker(period).count();
    let value = source.join_passive(&fed_back, |src, fb| src + fb * 10);
    let _loop = value.feedback(&sink);
    let acc = value.accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(period * 5)).unwrap();

    assert_eq!(vec![1, 12, 123, 1234, 12345, 123456], r.value(&acc));
}

/// Mirrors `feedback_active_works` (feedback.rs): `constant(1)` joined with
/// the fed-back value (`a + b*10`), the result fed back. Each cycle the
/// previous result re-enters scaled, giving 1, 11, 111, 1111, 11111.
#[test]
fn feedback_active_matches_classic_engine() {
    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();
    let one = g.constant(1u64);
    let sum = one.join(&fed_back, |a, b| a + b * 10);
    let writer = sum.feedback(&sink);
    let acc = writer.accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    assert_eq!(vec![1, 11, 111, 1111, 11111], r.value(&acc));
}

/// A self-driving feedback loop with no external ticker: `constant` fires
/// once, then the loop sustains itself — each cycle doubles the fed-back
/// value. Demonstrates that the sink's `+1` scheduling keeps the graph live
/// with no other source, and terminates cleanly at the cycle bound.
///
/// (A running total synchronised to a ticker needs *passive* feedback — a
/// join whose feedback input is read but doesn't trigger — which lands with
/// the passive-input `bimap` node in Phase 2.)
#[test]
fn feedback_self_sustains() {
    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();
    let seed = g.constant(1u64);
    let doubled = seed.join(&fed_back, |s, f| if *f == 0 { *s } else { f * 2 });
    let _loop = doubled.feedback(&sink);
    let acc = doubled.accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    // 1, then 2, 4, 8, 16.
    assert_eq!(vec![1, 2, 4, 8, 16], r.value(&acc));
}

/// The sink is clonable (one source fed from several sites) — mirrors
/// `feedback_sink_clone_works`.
#[test]
fn feedback_sink_clones() {
    let g = GraphBuilder::new();
    let (_src, sink) = g.feedback::<u64>();
    let _sink2 = sink.clone();
}
