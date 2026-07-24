//! Phase 2 node-catalog parity for the multi-input `try_*` combines and the
//! `print` / `timed` pass-through diagnostics. Each test mirrors the classic
//! node's own unit test (`try_bimap`, `try_trimap`, `print`, `timed`) —
//! reproducing the same values and, for the passive cases, the same tick
//! times.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

// --- try_join (classic `try_bimap`) ----------------------------------------

/// `try_join` combines two active streams with a fallible closure — mirrors
/// classic `try_bimap::try_bimap_success` (a + b*10, last = 55 at cycle 5).
#[test]
fn try_join_success() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let a = tick.count();
    let b = tick.count().map(|x| x * 10);
    let combined = a.try_join(&b, |a: &u64, b: &u64| Ok(a + b));
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(55, r.value(&combined));
}

/// A closure error aborts the run — mirrors classic
/// `try_bimap::try_bimap_error`.
#[test]
fn try_join_error_aborts_run() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let a = tick.count();
    let b = tick.count();
    let _combined = a.try_join(&b, |_: &u64, _: &u64| -> anyhow::Result<u64> {
        anyhow::bail!("oops")
    });
    let mut r = g.build();
    assert!(r.run(HISTORICAL, RunFor::Cycles(1)).is_err());
}

/// A passive `try_join` input is read but does not trigger — mirrors classic
/// `try_bimap::try_bimap_passive_does_not_trigger`. The combine fires only on
/// the active (slow, 100ns) input, at t = 0, 100, 200.
#[test]
fn try_join_passive_does_not_trigger() {
    let g = GraphBuilder::new();
    let a = g.ticker(Duration::from_nanos(100)).count(); // active
    let b = g.ticker(Duration::from_nanos(50)).count(); // passive
    let combined = a.try_join_passive(&b, |a: &u64, b: &u64| Ok(a + b));
    let times = combined.ticked_at().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)],
        r.value(&times)
    );
}

// --- try_join3 (classic `try_trimap`) --------------------------------------

/// `try_join3` combines three active streams with a fallible closure —
/// mirrors classic `try_trimap::try_trimap_success` (a + b*10 + c*100, last =
/// 555 at cycle 5).
#[test]
fn try_join3_success() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let a = tick.count();
    let b = tick.count().map(|x| x * 10);
    let c = tick.count().map(|x| x * 100);
    let combined = a.try_join3(&b, &c, |a: &u64, b: &u64, c: &u64| Ok(a + b + c));
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(555, r.value(&combined));
}

/// A closure error aborts the run — mirrors classic
/// `try_trimap::try_trimap_error`.
#[test]
fn try_join3_error_aborts_run() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let a = tick.count();
    let b = tick.count();
    let c = tick.count();
    let _combined = a.try_join3(&b, &c, |_: &u64, _: &u64, _: &u64| -> anyhow::Result<u64> {
        anyhow::bail!("oops")
    });
    let mut r = g.build();
    assert!(r.run(HISTORICAL, RunFor::Cycles(1)).is_err());
}

/// Passive `try_join3` inputs are read but do not trigger — mirrors classic
/// `try_trimap::try_trimap_passive_does_not_trigger`. Fires only on the active
/// (slow, 100ns) input, at t = 0, 100, 200.
#[test]
fn try_join3_passive_does_not_trigger() {
    let g = GraphBuilder::new();
    let a = g.ticker(Duration::from_nanos(100)).count(); // active
    let b = g.ticker(Duration::from_nanos(50)).count(); // passive
    let c = g.ticker(Duration::from_nanos(50)).count(); // passive
    // `try_join3` makes all three active; use the builder directly to keep b
    // and c passive, matching the classic test.
    let times = a
        .wire(|bld, h| {
            let (bh, ch) = (b.handle(), c.handle());
            bld.try_trimap(
                h,
                true,
                bh,
                false,
                ch,
                false,
                |a: &u64, b: &u64, c: &u64| Ok(a + b + c),
            )
        })
        .ticked_at()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)],
        r.value(&times)
    );
}

// --- print -----------------------------------------------------------------

/// `print` passes values through unchanged (the buffered print is a teardown
/// side effect) — mirrors classic `print::print_passes_through_values`.
#[test]
fn print_passes_through_values() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let acc = count.print().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], r.value(&acc));
}

// --- timed -----------------------------------------------------------------

/// `timed` passes values through unchanged (the summary is a stop side
/// effect) — mirrors classic `timed::timed_historical`.
#[test]
fn timed_passes_through_values() {
    let g = GraphBuilder::new();
    let out = g.ticker(Duration::from_nanos(100)).count().timed();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, r.value(&out));
}
