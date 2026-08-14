//! `Op::SEEDED_EDGES`: an op that reads a *held* value must not run before
//! that value is real.
//!
//! Every value slot is born holding `T::default()` (see `Builder::new_slot`
//! for why that bound is kept), and a seeded `0` is indistinguishable from a
//! produced `0`. Without a gate, the ops that read a value they did not
//! themselves cause — `sample`'s data leg, `join`'s other side — emit values
//! no source ever produced. In this crate's target domain that is a price of
//! `0.0` reaching a strategy, which is the worst kind of bug: silent,
//! plausible, and fatal to a backtest rather than to the process.
//!
//! `filter` was the first instance found (#440, fixed in #717 with a per-op
//! `State` flag). This file guards the rest of the class, which a `State` flag
//! *cannot* reach: a passive edge can tick in a cycle where its reader is not
//! activated at all, so the reader never observes the tick. See
//! `Op::SEEDED_EDGES`.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(10);

/// A stream that ticks from t=0 but produces nothing until its 6th value.
/// Anything reading it before then is reading a seed.
fn late(g: &GraphBuilder) -> Stream<u64> {
    g.ticker(P)
        .count()
        .filter_value(|n: &u64| *n > 5)
        .map(|n: &u64| n * 1000)
}

// ---------------------------------------------------------------------------
// The gated ops: nothing before the source is real.
// ---------------------------------------------------------------------------

#[test]
fn sample_stays_quiet_until_its_data_edge_produces() {
    let g = GraphBuilder::new();
    let acc = late(&g)
        .sample(&g.ticker(P).map(|_: &()| ()))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();

    // Pre-fix this was [0, 0, 0, 0, 0, 6000, ...] — five values from nowhere.
    assert_eq!(
        vec![
            (NanoTime::new(50), 6000),
            (NanoTime::new(60), 7000),
            (NanoTime::new(70), 8000),
            (NanoTime::new(80), 9000),
        ],
        r.value(&acc)
    );
}

#[test]
fn join_stays_quiet_until_both_edges_produce() {
    let g = GraphBuilder::new();
    let early = g.ticker(P).count();
    let acc = early
        .join(&late(&g), |a: &u64, b: &u64| (*a, *b))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();

    // Pre-fix: [(1,0), (2,0), (3,0), (4,0), (5,0), (6,6000), ...].
    assert_eq!(
        vec![(6, 6000), (7, 7000), (8, 8000), (9, 9000)],
        r.value(&acc)
    );
}

#[test]
fn try_join_stays_quiet_until_both_edges_produce() {
    let g = GraphBuilder::new();
    let early = g.ticker(P).count();
    let acc = early
        .try_join(&late(&g), |a: &u64, b: &u64| Ok((*a, *b)))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(
        vec![(6, 6000), (7, 7000), (8, 8000), (9, 9000)],
        r.value(&acc)
    );
}

#[test]
fn join_passive_stays_quiet_until_its_passive_edge_produces() {
    // The case a per-op `State` flag cannot fix: edge 1 is passive, so it can
    // tick in a cycle where this node is never activated, and a `State` flag
    // fed by tick observations would stay false forever.
    let g = GraphBuilder::new();
    let early = g.ticker(P).count();
    let acc = early
        .join_passive(&late(&g), |a: &u64, b: &u64| (*a, *b))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(
        vec![(6, 6000), (7, 7000), (8, 8000), (9, 9000)],
        r.value(&acc)
    );
}

#[test]
fn join3_stays_quiet_until_all_three_edges_produce() {
    let g = GraphBuilder::new();
    let early = g.ticker(P).count();
    let acc = early
        .join3(&early, &late(&g), |a: &u64, b: &u64, c: &u64| (*a, *b, *c))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(
        vec![(6, 6, 6000), (7, 7, 7000), (8, 8, 8000), (9, 9, 9000)],
        r.value(&acc)
    );
}

// ---------------------------------------------------------------------------
// What the gate must NOT break.
// ---------------------------------------------------------------------------

/// `merge` only ever emits a value that just ticked, so it was never in the
/// class and must not become gated by accident.
#[test]
fn merge_is_ungated_and_unchanged() {
    let g = GraphBuilder::new();
    let acc = g.ticker(P).count().merge(&late(&g)).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(vec![1, 2, 3, 4, 5, 6, 7, 8, 9], r.value(&acc));
}

/// A feedback edge is a *cycle*: its seed is the bootstrap, not a placeholder.
/// Gating a reader on it would wait for a value that cannot arrive until the
/// reader has itself ticked — a deadlock that shows up as empty output.
/// `Builder::feedback` marks its slot produced at wiring for exactly this.
#[test]
fn feedback_bootstrap_survives_the_gate() {
    let g = GraphBuilder::new();
    let (fed_back, sink) = g.feedback::<u64>();
    let seed = g.constant(1u64);
    // The join reads the feedback leg — the edge that would deadlock if its
    // bootstrap seed did not count as produced.
    let doubled = seed.join(
        &fed_back,
        |s: &u64, f: &u64| if *f == 0 { *s } else { f * 2 },
    );
    let _loop = doubled.feedback(&sink);
    let acc = doubled.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 2, 4, 8, 16], r.value(&acc));
}

/// `fold(init, ..)`'s seed is a caller-supplied *meaningful* value, not a
/// `Default::default()` placeholder — so a passive read of it before the first
/// tick legitimately observes `init`. That is what `SEEDED_INIT` distinguishes;
/// `tests/parity_bugs.rs::fold_non_default_init_seed_parity` pins the values.
#[test]
fn a_meaningful_init_seed_is_readable_before_the_first_tick() {
    let g = GraphBuilder::new();
    let base = g.ticker(P).count().map(|c: &u64| *c as i64);
    let acc = base
        .delay(Duration::from_nanos(25))
        .fold(100i64, |a: &mut i64, v: &i64| *a += v)
        .sample(&g.ticker(P))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        Some(&100),
        r.value(&acc).first(),
        "fold's init is a real value, not a seed to gate on"
    );
}

// ---------------------------------------------------------------------------
// Tier parity: the gate is a `const`, so all three engines must agree.
// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn gated(g: &GraphBuilder) -> Stream<u64> {
        let early = g.ticker(P).count();
        let late = early.filter_value(|n: &u64| *n > 5).map(|n: &u64| n * 1000);
        let out = early.join(&late, |a: &u64, b: &u64| a + b);
        out
    }
}

#[test]
fn all_three_tiers_agree_on_the_gate() {
    let run_for = RunFor::Cycles(9);

    let (mut runner, out) = gated::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);

    let (compiled,) = gated::compiled(HISTORICAL, run_for).unwrap();

    let g = GraphBuilder::new();
    let island = gated::nested(&g);
    let island_acc = island.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    let nested_last = r.value(&island_acc).last().copied();

    assert_eq!(
        interpreted, compiled,
        "interpreted and compiled must agree on the gate"
    );
    assert_eq!(
        Some(interpreted),
        nested_last,
        "the island must agree too — the gate folds from the same const"
    );
    // And the gate actually bit: the first five cycles produced nothing, so
    // the accumulated island output is shorter than the cycle count.
    assert!(
        r.value(&island_acc).len() < 9,
        "the gate should have suppressed the pre-first-value cycles"
    );
}
