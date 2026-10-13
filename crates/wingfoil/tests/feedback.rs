//! Phase 0.2 spike: feedback edges — the one true DAG-breaker. The source
//! stream has no upstreams (so the graph stays acyclic); the sink pushes onto
//! a shared time-queue and schedules the source to emit one cycle later. This
//! reproduces the legacy engine's `feedback_active_works` behaviour.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Mirrors `feedback_passive_works` (feedback.rs): a counter joined with a
/// *passively* read feedback value. The feedback input is read but does not
/// trigger, so the combine advances in step with the counter (once per
/// period) rather than on every feedback delivery — giving the digit-shift
/// progression 1, 12, 123, 1234, 12345, 123456.
#[test]
fn feedback_passive_matches_legacy_engine() {
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
fn feedback_active_matches_legacy_engine() {
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

/// A *conditional* send — the case legacy spelled `sink.send(value, &mut
/// GraphState)` from inside a running op. `map_filter` in front of
/// `stream.feedback(&sink)` is the whole replacement: only values that pass
/// the predicate leave, and the sink's `+1` schedule is what a reader gets
/// wrong, so it is pinned here — each value returns one nanosecond after it
/// was produced, not one ticker period later.
#[test]
fn feedback_conditional_send_maps_and_filters() {
    let period = Duration::from_nanos(100);
    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();

    // A ticker drives the loop; only even counts go round it, scaled by ten.
    let ticks = g.ticker(period).count();
    let out = ticks.map_filter(|n: &u64| (*n * 10, n.is_multiple_of(2)));
    let _loop = out.feedback(&sink);

    // The pass-through shows what was sent, the fed-back source what arrived.
    let sent = out.with_time().accumulate();
    let echoed = fed_back.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(
        vec![(NanoTime::new(100), 20), (NanoTime::new(300), 40)],
        r.value(&sent)
    );
    // Odd counts never leave, so 10 and 30 never appear; each value comes back
    // one nanosecond after the cycle that sent it.
    assert_eq!(
        vec![(NanoTime::new(101), 20), (NanoTime::new(301), 40)],
        r.value(&echoed)
    );
}

/// `feedback_loop` is the pair wired in one place, not a different edge: the
/// legacy passive progression above, built through the loop, is the pair's
/// value for value and tick for tick — and what comes back is the loop's
/// pass-through, the values sent round, at the instants they were sent.
#[test]
fn feedback_loop_is_the_pair_in_one_place() {
    let period = Duration::from_nanos(100);

    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();
    let value = g
        .ticker(period)
        .count()
        .join_passive(&fed_back, |src, fb| src + fb * 10);
    let paired = value.feedback(&sink).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(period * 5)).unwrap();
    let paired = r.value(&paired);

    let g = GraphBuilder::new();
    let source = g.ticker(period).count();
    let sent =
        g.feedback_loop(|fed_back| source.join_passive(fed_back, |src, fb: &u64| src + fb * 10));
    let looped = sent.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(period * 5)).unwrap();
    let looped = r.value(&looped);

    let expected: Vec<(NanoTime, u64)> = [1, 12, 123, 1234, 12345, 123456]
        .into_iter()
        .enumerate()
        .map(|(i, v)| (NanoTime::new(i as u64 * 100), v))
        .collect();
    assert_eq!(looped, expected);
    assert_eq!(looped, paired, "the loop and the pair agree");
}

/// A body that returns a `FeedbackLoop` hands its `out` back untouched beside the pass-through: here the active legacy loop, and a
/// stream derived from it inside the body that is read outside it.
#[test]
fn feedback_loop_hands_out_what_the_body_built() {
    let g = GraphBuilder::new();
    let one = g.constant(1u64);
    let FeedbackLoop {
        feedback: sum,
        out: doubled,
    } = g.feedback_loop(|fed_back| {
        let sum = one.join(fed_back, |a: &u64, b: &u64| a + b * 10);
        let doubled = sum.map(|s: &u64| s * 2);
        FeedbackLoop {
            feedback: sum,
            out: doubled,
        }
    });
    let sums = sum.with_time().accumulate();
    let doubled = doubled.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    let at = |v: [u64; 5]| -> Vec<(NanoTime, u64)> {
        v.into_iter()
            .enumerate()
            .map(|(i, v)| (NanoTime::new(i as u64), v))
            .collect()
    };
    // Each value comes back one nanosecond after it was sent.
    assert_eq!(r.value(&sums), at([1, 11, 111, 1111, 11111]));
    assert_eq!(r.value(&doubled), at([2, 22, 222, 2222, 22222]));
}

/// A body that wires something fallible returns a `Result` and uses `?`. On
/// `Ok` the loop closes as it always does; on `Err` the error comes back to
/// the caller, who must handle it — `Result` is `#[must_use]`.
#[test]
fn feedback_loop_body_can_fail() -> anyhow::Result<()> {
    let scale = |text: &str| -> anyhow::Result<u64> { Ok(text.parse()?) };

    let g = GraphBuilder::new();
    let one = g.constant(1u64);
    let sum = g.feedback_loop(|was| -> anyhow::Result<Stream<u64>> {
        let ten = scale("10")?;
        Ok(one.join(was, move |a: &u64, b: &u64| a + b * ten))
    })?;
    let sums = sum.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3))?;
    assert_eq!(
        r.value(&sums),
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(1), 11),
            (NanoTime::new(2), 111)
        ]
    );

    let g = GraphBuilder::new();
    let failed = g.feedback_loop(|was: &Stream<u64>| -> anyhow::Result<Stream<u64>> {
        let ten = scale("ten")?;
        Ok(was.map(move |v: &u64| v * ten))
    });
    assert!(failed.is_err(), "the body's error comes back");
    Ok(())
}
