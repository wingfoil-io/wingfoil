//! Regression tests for the semantic-parity bugs found in the fable review
//! (`docs/fable-review.md`). Each test pins interpreted == compiled == nested
//! for a case that previously drifted between the three execution paths, or
//! pins next's behaviour against classic wingfoil.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

// ===========================================================================
// BUG 1: Fold value-slot seeding drift.
//
// The interpreted engine seeds a fold's output slot with `init.clone()`; the
// compiled/nested emission used to seed every slot with `Default::default()`.
// A fold with `init != Default` read (via a passive/sample edge) before its
// first tick therefore returned `init` interpreted but `0` compiled/nested.
// ===========================================================================

wingfoil_next::graph! {
    fn fold_seed(g: &GraphBuilder) -> Stream<Vec<i64>> {
        // A trigger that ticks from t=0, and a fold whose source is *delayed*
        // so the fold does not tick until t=25. Sampling the fold on the
        // trigger reads its output slot at t=0/10/20 — before its first
        // tick — so the read observes the seed, not a folded value.
        let trig = g.ticker(Duration::from_nanos(10));
        let base = g.ticker(Duration::from_nanos(10)).count().map(|c| *c as i64);
        let delayed = base.delay(Duration::from_nanos(25));
        let acc = delayed.fold(100i64, |a, v| *a += v);
        let sampled = acc.sample(&trig).accumulate();
        sampled
    }
}

#[test]
fn fold_non_default_init_seed_parity() {
    let run_for = RunFor::Cycles(6);

    let (mut runner, sampled) = fold_seed::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(sampled);

    // The fold seeds with init=100 and does not tick until the delay elapses,
    // so the earliest passive reads observe 100 (not Default = 0).
    assert_eq!(100, interpreted[0], "passive read before first tick sees init");

    let (compiled,) = fold_seed::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled, "interpreted == compiled");

    let g = GraphBuilder::new();
    let island = fold_seed::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
}

// ===========================================================================
// BUG 6: reachable user errors must `bail!`, not panic.
//
// Running a graph with external/poll sources historically, or running a
// realtime-source graph twice, is a reachable caller mistake — it must return
// an `Err`, not `assert!`/`.expect()`-panic.
// ===========================================================================

#[test]
fn external_source_historical_run_is_an_error_not_a_panic() {
    let g = GraphBuilder::new();
    let (values, _src) = g.external::<u64>();
    let _acc = values.collapse_accumulate();
    let mut r = g.build();

    let err = r
        .run(HISTORICAL, RunFor::Cycles(3))
        .expect_err("external source in a historical run must error");
    assert!(
        format!("{err:#}").contains("RunMode::RealTime"),
        "error explains the realtime requirement: {err:#}"
    );
}

#[test]
fn poll_source_historical_run_is_an_error_not_a_panic() {
    let g = GraphBuilder::new();
    let _p = g.poll(|| Some(1u64)).accumulate();
    let mut r = g.build();

    let err = r
        .run(HISTORICAL, RunFor::Cycles(3))
        .expect_err("poll source in a historical run must error");
    assert!(
        format!("{err:#}").contains("busy-poll"),
        "error explains the poll/realtime requirement: {err:#}"
    );
}
