//! Re-run / runner-lifecycle parity (spike 0.4 → Phase 1 contract).
//!
//! A [`Runner`](wingfoil::interp::Runner) for a *re-runnable* graph
//! (tickers/constants + combinators + feedback — the deterministic historical
//! subset) may be run repeatedly. Each `run` first restores every node's
//! engine-owned state and value slot to its wiring-time initial value, so two
//! runs of the same runner are independent and reproduce a freshly-built graph
//! exactly — legacy wingfoil's setup-per-run semantics. This is the contract
//! the wingfoil-python re-run depends on; spike 0.4 deferred it to Phase 1,
//! and here it is.
//!
//! Single-run graphs (external/poll/channel sources, nested islands) hold state
//! the engine cannot reset — their producer channels, wakers, and island
//! interiors are consumed by the first run — so a second `run` errors rather
//! than silently producing wrong values.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const TEN: Duration = Duration::from_nanos(10);

/// A graph exercising stateful ops whose state *must* reset between runs: a
/// `count` (fold), a `map`, a running `fold`, a `delay` (self-scheduling queue),
/// and a `merge`. Returns the accumulated merged output.
fn wire(g: &GraphBuilder) -> Stream<Vec<u64>> {
    let count = g.ticker(TEN).count(); // 1, 2, 3, ...
    let doubled = count.map(|i| i * 2); // 2, 4, 6, ...
    let running = doubled.fold(0u64, |acc, x| *acc += *x); // 2, 6, 12, ...
    let delayed = count.delay(Duration::from_nanos(25)); // count, shifted later
    running.merge(&delayed).accumulate()
}

/// The core contract: a second `run` of the same runner reproduces the first
/// exactly (state resets — the running fold restarts from its seed rather than
/// continuing), and both equal a freshly-built graph run once.
#[test]
fn rerun_matches_fresh_graph() {
    let g = GraphBuilder::new();
    let acc = wire(&g);
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let first = r.value(&acc);

    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let second = r.value(&acc);

    // A freshly-built identical graph is the parity oracle.
    let g2 = GraphBuilder::new();
    let acc2 = wire(&g2);
    let mut r2 = g2.build();
    r2.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let fresh = r2.value(&acc2);

    assert_eq!(
        first, second,
        "a re-run must reproduce the first run exactly"
    );
    assert_eq!(first, fresh, "a re-run must match a freshly-built graph");
    assert!(
        !first.is_empty(),
        "sanity: the graph actually produced values"
    );
}

/// Without the reset, a running fold would *continue* across runs (spike 0.4's
/// "counter goes 3 → 6"). Assert it restarts instead.
#[test]
fn fold_accumulator_restarts_not_continues() {
    let g = GraphBuilder::new();
    let sum = g.ticker(TEN).count().fold(0u64, |acc, x| *acc += *x);
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(1 + 2 + 3, r.value(&sum)); // 6

    // If state leaked, the second run would end at 6 + (1+2+3) = 12.
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(1 + 2 + 3, r.value(&sum)); // 6 again, not 12
}

#[test]
fn step_by_restarts_from_index_zero() {
    let g = GraphBuilder::new();
    let stepped = g.ticker(TEN).count().step_by(3).with_time().accumulate();
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(7)).unwrap();
    let first = r.value(&stepped);
    r.run(HISTORICAL, RunFor::Cycles(7)).unwrap();

    assert_eq!(
        vec![
            (NanoTime::new(0), 1u64),
            (NanoTime::new(30), 4),
            (NanoTime::new(60), 7),
        ],
        first
    );
    assert_eq!(first, r.value(&stepped));
}

/// The buffering / scheduling / passive-read ops (`window`, `throttle`,
/// `debounce`, `sample`, `with_time`) each reset their state and value slot
/// between runs.
#[test]
fn buffering_and_sampling_ops_reset() {
    let g = GraphBuilder::new();
    let count = g.ticker(TEN).count();
    let windowed = count.window(Duration::from_nanos(30)).accumulate();
    let throttled = count.throttle(Duration::from_nanos(30)).accumulate();
    let debounced = count.debounce(Duration::from_nanos(25)).accumulate();
    let sampled = count
        .sample(&g.ticker(Duration::from_nanos(20)))
        .accumulate();
    let timed = count.with_time().accumulate();
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    let (w1, t1, d1, s1, ts1) = (
        r.value(&windowed),
        r.value(&throttled),
        r.value(&debounced),
        r.value(&sampled),
        r.value(&timed),
    );

    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    assert_eq!(w1, r.value(&windowed), "window must reset");
    assert_eq!(t1, r.value(&throttled), "throttle must reset");
    assert_eq!(d1, r.value(&debounced), "debounce must reset");
    assert_eq!(s1, r.value(&sampled), "sample must reset");
    assert_eq!(ts1, r.value(&timed), "with_time must reset");
}

/// Feedback graphs re-run: the shared time-queue is cleared and the source slot
/// reset, so the legacy `1, 11, 111, ...` sequence reproduces on every run.
#[test]
fn feedback_reruns() {
    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();
    let one = g.constant(1u64);
    let sum = one.join(&fed_back, |a, b| a + b * 10);
    let writer = sum.feedback(&sink);
    let acc = writer.accumulate();
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 11, 111, 1111, 11111], r.value(&acc));

    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 11, 111, 1111, 11111], r.value(&acc));
}

/// An explicit [`Runner::reset`] restores state without running; a subsequent
/// `run` then starts clean, same as the auto-reset a re-run performs.
#[test]
fn explicit_reset_restores_state() {
    let g = GraphBuilder::new();
    let sum = g.ticker(TEN).count().fold(0u64, |acc, x| *acc += *x);
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(1 + 2 + 3 + 4, r.value(&sum)); // 10
    r.reset();
    // After reset (before any re-run) the slot is back to the fold seed.
    assert_eq!(0, r.value(&sum));

    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(10, r.value(&sum));
}

#[test]
fn enumerate_restarts_at_zero_on_rerun() {
    let g = GraphBuilder::new();
    let indexed = g.ticker(TEN).count().enumerate().accumulate();
    let mut r = g.build();
    let expected = vec![(0u64, 1u64), (1, 2), (2, 3)];

    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(expected, r.value(&indexed));

    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(expected, r.value(&indexed));
}

#[test]
fn start_with_reemits_initial_value_on_rerun() {
    let g = GraphBuilder::new();
    let out = g
        .constant(7u64)
        .delay(Duration::from_nanos(5))
        .start_with(1)
        .with_time()
        .accumulate();
    let mut r = g.build();
    let expected = vec![(NanoTime::ZERO, 1u64), (NanoTime::new(5), 7)];

    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();
    assert_eq!(expected, r.value(&out));

    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();
    assert_eq!(expected, r.value(&out));
}

/// A single-run graph (here a busy-poll source) cannot reset — its producer
/// channel is consumed by the first run — so a second `run` errors with a
/// clear message rather than silently producing wrong values.
#[test]
fn single_run_graph_errors_on_second_run() {
    let g = GraphBuilder::new();
    let polled = g.poll(|| None::<u64>);
    let _acc = polled.accumulate();
    let mut r = g.build();

    r.run(RunMode::RealTime, RunFor::Cycles(4)).unwrap();
    let err = r
        .run(RunMode::RealTime, RunFor::Cycles(4))
        .expect_err("a single-run graph must refuse a second run");
    assert!(
        err.to_string().contains("single-run"),
        "unexpected error: {err}"
    );
}

/// `merge_all` (one n-ary merge node) produces exactly what the left-associated
/// chain of 2-ary merges it replaced did — same values, same earliest-wins
/// tie-break — and both re-run cleanly. The *shape* difference the rewrite was
/// for (one node, not `n - 1`) is gated in `merge_n.rs`; this is the
/// behavioural half.
#[test]
fn merge_all_matches_chained_merge() {
    fn wire_nary(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let base = g.ticker(TEN).count();
        let a = base.map(|i| i * 10);
        let b = base.map(|i| i * 100);
        let c = base.map(|i| i * 1000);
        a.merge_all(&[&b, &c]).accumulate()
    }
    fn wire_chained(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let base = g.ticker(TEN).count();
        let a = base.map(|i| i * 10);
        let b = base.map(|i| i * 100);
        let c = base.map(|i| i * 1000);
        a.merge(&b).merge(&c).accumulate()
    }

    let gn = GraphBuilder::new();
    let nary = wire_nary(&gn);
    let mut rn = gn.build();
    rn.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    let gc = GraphBuilder::new();
    let chained = wire_chained(&gc);
    let mut rc = gc.build();
    rc.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    assert_eq!(rn.value(&nary), rc.value(&chained));
    assert!(!rn.value(&nary).is_empty());

    // `merge_all(&[])` is just the stream itself.
    let g0 = GraphBuilder::new();
    let base = g0.ticker(TEN).count();
    let lone = base.map(|i| i * 10);
    let empty = lone.merge_all(&[]).accumulate();
    let direct = lone.accumulate();
    let mut r0 = g0.build();
    r0.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(r0.value(&direct), r0.value(&empty));
}
