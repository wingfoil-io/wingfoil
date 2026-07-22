//! Parity tests for the **time-weighted, time-windowed** moments
//! (`time_weighted_windowed_mean` / `var` / `std`), ported from the classic
//! `adapters::statistics` `RollingMomentStream` under `Weighting::Time` over a
//! `Window::Time` (`wingfoil/src/adapters/statistics.rs` ~401). This is the last
//! statistics combo: Δt-weighting (as in the cumulative time-weighted ops) plus
//! window eviction (as in the count-weighted time-windowed ops).
//!
//! Exact eviction / reweight semantics reproduced from the classic node:
//!
//! * on each tick the **previous** sample's interval is closed — it is credited
//!   with the elapsed Δt (`now - prev_t`) via West's weighted-Welford `push`;
//!   the new sample contributes nothing until the next tick advances the clock
//!   (a left-continuous step signal);
//! * front entries whose age `now - t` is **strictly greater** than the window
//!   are evicted, and each evicted sample's **entire** committed interval weight
//!   (`next_t - old_t`, the gap to its successor) is removed via the exact West
//!   `remove` — classic does **not** clip the aging boundary sample's interval
//!   to the window edge (a sample is wholly weighted or wholly evicted);
//! * variance is the **population** form `m2 / Σ Δt` (time weighting has no clean
//!   ddof correction); the first sample seeds the mean to the raw value and the
//!   variance to `0.0`.
//!
//! The fixture drives an **uneven** `(value, time)` grid (a dropped tick makes
//! the Δt weights unequal) with a window narrow enough that samples **age out
//! mid-stream** — so the result provably differs from BOTH the count-weighted
//! time-windowed mean AND the cumulative time-weighted mean (hand-computed
//! contrasts are asserted). A direct two-pass recompute over the retained,
//! Δt-weighted window cross-checks the incremental add/remove.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::Stream;
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// An uneven `(value, time)` grid: on a 100ns tick grid (count 1..6) the tick at
/// t = 300ns (n = 4) is dropped, so values 1,2,3,4,5 land at
/// t = 0,100,200,400,500ns. Gaps between consecutive fired ticks are therefore
/// 100,100,200,100 ns — uneven, so Δt weighting differs from count weighting.
///
/// `map_filter` renumbers around the dropped tick: n=1→1, n=2→2, n=3→3,
/// (n=4 dropped), n=5→4, n=6→5.
fn uneven(g: &GraphBuilder) -> Stream<f64> {
    g.ticker(Duration::from_nanos(100)).count().map_filter(|n| {
        let emit = *n != 4;
        let value = (if *n < 4 { *n } else { *n - 1 }) as f64;
        (value, emit)
    })
}

/// A 350ns window over the uneven grid retains a bounded trailing slice.
const WIN: Duration = Duration::from_nanos(350);

/// Assert two `f64` series equal element-wise within a tolerance.
fn assert_series_approx(got: &[f64], expected: &[f64]) {
    assert_eq!(got.len(), expected.len(), "length: {got:?} vs {expected:?}");
    for (i, (g, e)) in got.iter().zip(expected).enumerate() {
        assert!(
            (g - e).abs() < 1e-9,
            "at {i}: got {g}, expected {e} (series {got:?} vs {expected:?})"
        );
    }
}

// ── time_weighted_windowed_mean ──────────────────────────────────────────────

/// Values 1,2,3,4,5 at t = 0,100,200,400,500ns; window 350ns. Committed weights
/// are the gaps to the successor: v1→100, v2→100, v3→200, v4→100 (v5 uncommitted).
/// Walking the observe+evict logic:
///   t0   : seed                                   → mean 1
///   t100 : {1:100}                                → mean 1
///   t200 : {1:100, 2:100}                         → mean (100+200)/200 = 1.5
///   t400 : commit v3 (Δt 200) then v1 ages out    → {2:100, 3:200};
///           mean = (2·100 + 3·200)/300 = 800/300 = 8/3
///   t500 : commit v4 (Δt 100) then v2 ages out    → {3:200, 4:100};
///           mean = (3·200 + 4·100)/300 = 1000/300 = 10/3
/// The final 10/3 differs from BOTH the count-weighted time-windowed mean over
/// the same window ({3,4,5} → 4.0) and the cumulative time-weighted mean over
/// all history ({1:100,2:100,3:200,4:100} → 1300/500 = 2.6) — so BOTH the
/// uneven Δt weighting AND the mid-stream eviction are doing real work.
#[test]
fn time_weighted_windowed_mean_uneven_with_eviction() {
    let g = GraphBuilder::new();
    let m = uneven(&g).time_weighted_windowed_mean(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let series = r.value(&m);
    assert_series_approx(&series, &[1.0, 1.0, 1.5, 8.0 / 3.0, 10.0 / 3.0]);

    let final_mean = *series.last().unwrap();
    // Distinct from count-weighted time-windowed mean of {3,4,5} = 4.0 ...
    assert!(
        (final_mean - 4.0).abs() > 0.4,
        "time weighting must diverge from count-weighted windowed mean (4.0)"
    );
    // ... and from the cumulative (un-windowed) time-weighted mean = 2.6.
    assert!(
        (final_mean - 2.6).abs() > 0.4,
        "eviction must diverge from cumulative time-weighted mean (2.6)"
    );
}

// ── time_weighted_windowed_var / std ─────────────────────────────────────────

/// Same grid/window. Population variance `m2 / Σ Δt`:
///   t0   : 0                              (empty)
///   t100 : {1:100}            mean 1       → 0
///   t200 : {1:100, 2:100}     mean 1.5     → m2 = 100·0.25·2 = 50; var 50/200 = 0.25
///   t400 : {2:100, 3:200}     mean 8/3     → m2 = 100·(2-8/3)² + 200·(3-8/3)²
///           = 100·(4/9) + 200·(1/9) = 600/9; var = (600/9)/300 = 2/9
///   t500 : {3:200, 4:100}     mean 10/3    → m2 = 200·(3-10/3)² + 100·(4-10/3)²
///           = 200·(1/9) + 100·(4/9) = 600/9; var = 2/9
/// `std` is the element-wise square root.
#[test]
fn time_weighted_windowed_var_std_uneven_with_eviction() {
    let g = GraphBuilder::new();
    let var = uneven(&g).time_weighted_windowed_var(WIN).accumulate();
    let std = uneven(&g).time_weighted_windowed_std(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let expected_var = [0.0, 0.0, 0.25, 2.0 / 9.0, 2.0 / 9.0];
    assert_series_approx(&r.value(&var), &expected_var);
    let expected_std: Vec<f64> = expected_var.iter().map(|v| v.sqrt()).collect();
    assert_series_approx(&r.value(&std), &expected_std);
}

// ── first-sample / constant edge cases ───────────────────────────────────────

/// The first sample seeds the tracker with no weight: mean = raw sample,
/// variance = `0.0` (not `NaN`).
#[test]
fn time_weighted_windowed_first_sample_seeds() {
    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .time_weighted_windowed_mean(WIN);
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .time_weighted_windowed_var(WIN);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    assert_eq!(7.0, r.value(&mean));
    assert_eq!(0.0, r.value(&var));
}

/// A constant stream is time-weighted to the same constant with zero variance;
/// `std` must clamp at zero (never `NaN`), even as samples age through the
/// window.
#[test]
fn time_weighted_windowed_constant_is_constant() {
    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 3.5)
        .time_weighted_windowed_mean(WIN)
        .accumulate();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 3.5)
        .time_weighted_windowed_std(WIN)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    for v in r.value(&mean) {
        assert!((v - 3.5).abs() < 1e-10, "mean should stay 3.5, got {v}");
    }
    for v in r.value(&std) {
        assert!(!v.is_nan(), "std must not be NaN");
        assert!(v.abs() < 1e-10, "constant std should be 0.0, got {v}");
    }
}

// ── direct-recompute cross-check ─────────────────────────────────────────────

/// The incremental add/remove (West) must match an independent **two-pass**
/// recompute over the retained, Δt-weighted window: at the final tick the
/// weighted set is every consecutive pair of *retained* fired ticks (a sample
/// weighted by the gap to its successor), where "retained" is age ≤ window. This
/// exercises both the reweight-on-eviction and the aging-boundary edge case.
#[test]
fn time_weighted_windowed_moments_match_direct_recompute() {
    const N: u32 = 60;
    const WIN_NS: u64 = 450;
    let win = Duration::from_nanos(WIN_NS);
    // Drop every 3rd tick, so the surviving ticks are unevenly spaced.
    let dropped = |n: u64| n.is_multiple_of(3);
    let seq = |n: u64| ((n * 7) % 13) as f64;

    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map_filter(move |n| (seq(*n), !dropped(*n)))
        .time_weighted_windowed_mean(win);
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map_filter(move |n| (seq(*n), !dropped(*n)))
        .time_weighted_windowed_var(win);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // Fired ticks: value seq(n) at time (n-1)*100ns for every non-dropped n.
    let fired: Vec<(u64, f64)> = (1..=N as u64)
        .filter(|n| !dropped(*n))
        .map(|n| ((n - 1) * 100, seq(n)))
        .collect();
    let final_now = fired.last().unwrap().0;
    // Retained = fired ticks within the window at the final tick (age <= WIN_NS).
    let retained: Vec<(u64, f64)> = fired
        .iter()
        .copied()
        .filter(|(t, _)| final_now - t <= WIN_NS)
        .collect();
    // Committed contributions = each retained sample weighted by the gap to its
    // (retained) successor; the last retained sample is uncommitted.
    let mut contribs: Vec<(f64, f64)> = Vec::new();
    for w in retained.windows(2) {
        let (t_prev, v_prev) = w[0];
        let (t_now, _) = w[1];
        contribs.push((v_prev, (t_now - t_prev) as f64));
    }
    let w_sum: f64 = contribs.iter().map(|&(_, w)| w).sum();
    let expected_mean = contribs.iter().map(|&(v, w)| v * w).sum::<f64>() / w_sum;
    let expected_var = contribs
        .iter()
        .map(|&(v, w)| w * (v - expected_mean).powi(2))
        .sum::<f64>()
        / w_sum;

    assert!(
        (r.value(&mean) - expected_mean).abs() < 1e-9,
        "mean {} vs {expected_mean}",
        r.value(&mean)
    );
    assert!(
        (r.value(&var) - expected_var).abs() < 1e-9,
        "var {} vs {expected_var}",
        r.value(&var)
    );
}

// ── tick-time parity ─────────────────────────────────────────────────────────

/// Ticks once per fired upstream tick (the dropped tick does not fire), so the
/// emitted times are exactly the surviving ticks: 0,100,200,400,500ns. Pin both
/// the tick times and the paired time-weighted-windowed mean.
#[test]
fn time_weighted_windowed_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = uneven(&g)
        .time_weighted_windowed_mean(WIN)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let series = r.value(&acc);
    let times: Vec<u64> = series.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![0, 100, 200, 400, 500], times);
    assert_series_approx(&values, &[1.0, 1.0, 1.5, 8.0 / 3.0, 10.0 / 3.0]);
}
