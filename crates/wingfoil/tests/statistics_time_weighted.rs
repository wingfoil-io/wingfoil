#![cfg(feature = "statistics")]

//! Parity tests for the **time-weighted** moment statistics
//! (`{cumulative,rolling,time_windowed}_{mean,var,std}_time_weighted`), ported
//! from the legacy `adapters::statistics` operators under `Weighting::Time`
//! (the legacy `MomentStream` for the unbounded window and
//! `RollingMomentStream` for the count and time windows,
//! `legacy/wingfoil/src/adapters/statistics.rs`).
//!
//! Time-weighting semantics reproduced (legacy `Weighting::Time`):
//!
//! * each sample is weighted by how long it was in effect — the elapsed Δt
//!   until its successor, read from the graph clock. On every tick the
//!   *previous* value is credited with the Δt before the new value takes over
//!   (a left-continuous step signal), so the newest sample contributes nothing
//!   until the next tick advances the clock;
//! * the **mean** seeds to the current sample until any weight has accumulated
//!   (rather than emitting `NaN`);
//! * the **variance** is the time-weighted *population* form — `m2 / w_sum`,
//!   with no ddof correction (reliability-weighted variance has no clean ddof) —
//!   and `0.0` until weight is present; `std` clamps at zero, never `NaN`;
//! * a windowed sample's committed interval is reverted when it leaves the
//!   window (count window: more than `window` samples retained; time window:
//!   aged strictly past `window` nanoseconds).
//!
//! Ticks are 100ns apart. The legacy parity references are the unit tests
//! `mean_time_weighted_lags_by_one_interval`,
//! `variance_time_weighted_is_population_over_weight`,
//! `rolling_mean_over_time_window_count_and_time` (time case), and
//! `rolling_var_std_over_time_window_time_weighted` in
//! `legacy/wingfoil/src/adapters/statistics.rs`.

use std::time::Duration;

use wingfoil::adapters::statistics::StatisticsOps;
use wingfoil::fluent::Stream;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A 250ns window over 100ns-spaced ticks retains the three most recent samples.
const WIN: Duration = Duration::from_nanos(250);

/// 1, 2, 3, 4, 5 as `f64`, one tick every 100ns (ticks at 0,100,200,300,400ns).
fn counter_f64(g: &GraphBuilder) -> Stream<f64> {
    g.ticker(Duration::from_nanos(100))
        .count()
        .map(|i| *i as f64)
}

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

/// Direct (from-scratch) time-weighted population mean and variance over a set
/// of `(value, time_ns)` samples sorted by time: each sample is weighted by the
/// gap to its successor; the last (newest) sample has no successor and so
/// contributes zero weight — exactly the legacy committed-interval semantics.
/// Used as an independent oracle for the incremental moments.
fn brute_time_weighted(samples: &[(f64, u64)]) -> (f64, f64) {
    let mut w_sum = 0.0;
    let mut mean = 0.0;
    let mut m2 = 0.0;
    for w in samples.windows(2) {
        let (v, t) = w[0];
        let (_, next_t) = w[1];
        let weight = (next_t - t) as f64;
        if weight <= 0.0 {
            continue;
        }
        w_sum += weight;
        let mean_old = mean;
        mean += (weight / w_sum) * (v - mean_old);
        m2 += weight * (v - mean_old) * (v - mean);
    }
    let var = if w_sum <= 0.0 { 0.0 } else { m2 / w_sum };
    (mean, var)
}

// ── cumulative time-weighted (unbounded window) ──────────────────────────────

/// Mirrors legacy `mean_time_weighted_lags_by_one_interval` (final value 2.5),
/// pinned as a full series. Ticks are evenly spaced, so each of 1,2,3,4 is in
/// effect for one 100ns interval before the next; 5 has not yet been credited.
/// The cumulative time-weighted mean therefore lags the count mean by one
/// interval: 1, 1, 1.5, 2, 2.5 (vs the count mean's 1, 1.5, 2, 2.5, 3).
#[test]
fn cumulative_mean_time_weighted_lags_by_one_interval() {
    let g = GraphBuilder::new();
    let mean = counter_f64(&g).cumulative_mean_time_weighted().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&mean), &[1.0, 1.0, 1.5, 2.0, 2.5]);
}

/// Mirrors legacy `variance_time_weighted_is_population_over_weight` (final
/// value 1.25), pinned as a full series. The credited values accumulate as
/// {1:100}, {1:100,2:100}, … so the time-weighted population variance
/// (`m2 / w_sum`) walks 0, 0, 0.25, 2/3, 1.25; at the last tick the credited
/// {1,2,3,4} each weight 100 give mean 2.5, m2 = 100·(2.25+0.25+0.25+2.25) =
/// 500, var = 500/400 = 1.25. `std` is the element-wise root.
#[test]
fn cumulative_var_std_time_weighted() {
    let g = GraphBuilder::new();
    let var = counter_f64(&g).cumulative_var_time_weighted().accumulate();
    let std = counter_f64(&g).cumulative_std_time_weighted().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let expected_var = [0.0, 0.0, 0.25, 2.0 / 3.0, 1.25];
    assert_series_approx(&r.value(&var), &expected_var);
    let expected_std: Vec<f64> = expected_var.iter().map(|v| v.sqrt()).collect();
    assert_series_approx(&r.value(&std), &expected_std);
}

/// A constant stream has zero time-weighted variance; revert-scheme drift can
/// make it a hair negative, so `std` must clamp at zero, not emit `NaN`.
#[test]
fn cumulative_std_time_weighted_of_constant_is_zero_not_nan() {
    let g = GraphBuilder::new();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .cumulative_std_time_weighted()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    for v in r.value(&std) {
        assert!(!v.is_nan(), "std must not be NaN");
        assert!(
            v.abs() < 1e-10,
            "constant stream std should be 0.0, got {v}"
        );
    }
}

// ── count-windowed rolling time-weighted ─────────────────────────────────────

/// Time-weighted mean/var over a count window of 4 on evenly-spaced ticks. Each
/// retained sample except the newest is credited one 100ns interval, so the
/// statistic is the population mean/var of the retained window with the newest
/// value dropped. Over 1,2,3,4,5 the windows are {1},{1,2},{1,2,3},{1,2,3,4},
/// {2,3,4,5}; dropping the newest leaves {},{1},{1,2},{1,2,3},{2,3,4} →
/// means (seed) 1, 1, 1.5, 2, 3 and final population variance of {2,3,4} =
/// 2/3.
#[test]
fn rolling_mean_var_time_weighted_hand_computed() {
    let g = GraphBuilder::new();
    let mean = counter_f64(&g).rolling_mean_time_weighted(4).accumulate();
    let var = counter_f64(&g).rolling_var_time_weighted(4).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&mean), &[1.0, 1.0, 1.5, 2.0, 3.0]);
    // {}→0, {1}→0, {1,2}→0.25, {1,2,3}→2/3, {2,3,4}→2/3.
    assert_series_approx(&r.value(&var), &[0.0, 0.0, 0.25, 2.0 / 3.0, 2.0 / 3.0]);
}

/// The incremental add/remove moments must match a direct recompute over the
/// retained count window (i.e. eviction has not drifted): slide a window of 10
/// over a non-trivial sequence and compare the final mean/variance to a
/// from-scratch time-weighted computation over exactly the retained samples.
#[test]
fn rolling_time_weighted_moments_match_direct_recompute() {
    const N: u32 = 200;
    const W: usize = 10;
    let seq = |n: u64| ((n % 7) as f64) * 1.5 - 3.0;

    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .rolling_mean_time_weighted(W);
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .rolling_var_time_weighted(W);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // count() emits 1..=N at times 0,100,..,(N-1)*100; the count window retains
    // the last W samples.
    let retained: Vec<(f64, u64)> = ((N as u64 - W as u64 + 1)..=N as u64)
        .map(|n| (seq(n), (n - 1) * 100))
        .collect();
    let (em, ev) = brute_time_weighted(&retained);
    assert!(
        (r.value(&mean) - em).abs() < 1e-9,
        "mean {} vs {em}",
        r.value(&mean)
    );
    assert!(
        (r.value(&var) - ev).abs() < 1e-9,
        "var {} vs {ev}",
        r.value(&var)
    );
}

/// A count window wide enough never to evict must equal the cumulative
/// (unbounded) time-weighted statistic tick-for-tick — the two share the same
/// `WeightedMoments`, differing only in eviction, which a large window disables.
#[test]
fn wide_count_window_time_weighted_matches_cumulative() {
    let g = GraphBuilder::new();
    let rolling = counter_f64(&g)
        .rolling_mean_time_weighted(1_000)
        .accumulate();
    let cumulative = counter_f64(&g).cumulative_mean_time_weighted().accumulate();
    let rolling_var = counter_f64(&g)
        .rolling_var_time_weighted(1_000)
        .accumulate();
    let cumulative_var = counter_f64(&g).cumulative_var_time_weighted().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&rolling), &r.value(&cumulative));
    assert_series_approx(&r.value(&rolling_var), &r.value(&cumulative_var));
}

// ── time-windowed rolling time-weighted ──────────────────────────────────────

/// Mirrors legacy `rolling_mean_over_time_window_count_and_time` (time case,
/// final 3.5), pinned as a full series. With WIN = 250ns the window holds the
/// last three samples; each is credited its 100ns interval except the newest.
/// The series is 1, 1, 1.5, 2.5, 3.5 — at the last tick the window is
/// {3@200,4@300,5@400}: 3 and 4 each held 100ns, 5 is "now" (Δt 0), so
/// TWA = (3·100 + 4·100)/200 = 3.5.
#[test]
fn time_windowed_mean_time_weighted_counter() {
    let g = GraphBuilder::new();
    let mean = counter_f64(&g)
        .time_windowed_mean_time_weighted(WIN)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&mean), &[1.0, 1.0, 1.5, 2.5, 3.5]);
}

/// Mirrors legacy `rolling_var_std_over_time_window_time_weighted` (final var
/// 0.25, std 0.5), pinned as a full series. From the third tick on the two
/// committed values in the window differ by one and each hold 100ns, so the
/// time-weighted population variance is m2/w_sum = 50/200 = 0.25 and std 0.5:
/// var 0, 0, 0.25, 0.25, 0.25; std 0, 0, 0.5, 0.5, 0.5.
#[test]
fn time_windowed_var_std_time_weighted_counter() {
    let g = GraphBuilder::new();
    let var = counter_f64(&g)
        .time_windowed_var_time_weighted(WIN)
        .accumulate();
    let std = counter_f64(&g)
        .time_windowed_std_time_weighted(WIN)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&var), &[0.0, 0.0, 0.25, 0.25, 0.25]);
    assert_series_approx(&r.value(&std), &[0.0, 0.0, 0.5, 0.5, 0.5]);
}

/// The incremental add/remove moments must match a direct recompute over the
/// retained *time* window: slide a 350ns window over a non-trivial sequence and
/// compare the final mean/variance to a from-scratch time-weighted computation
/// over exactly the samples still in the window.
#[test]
fn time_windowed_time_weighted_moments_match_direct_recompute() {
    const N: u32 = 60;
    // 350ns window over 100ns ticks retains ages 0,100,200,300 → four samples.
    const WIN_NS: u64 = 350;
    let win = Duration::from_nanos(WIN_NS);
    let seq = |n: u64| ((n % 7) as f64) * 1.5 - 3.0;

    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .time_windowed_mean_time_weighted(win);
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .time_windowed_var_time_weighted(win);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // count() emits 1..=N at times 0,100,..,(N-1)*100; final now = (N-1)*100.
    // Retained = samples within WIN_NS of now (age <= WIN_NS).
    let now = (N as u64 - 1) * 100;
    let retained: Vec<(f64, u64)> = (1..=N as u64)
        .map(|n| (n, (n - 1) * 100))
        .filter(|&(_, t)| now - t <= WIN_NS)
        .map(|(n, t)| (seq(n), t))
        .collect();
    let (em, ev) = brute_time_weighted(&retained);
    assert!(
        (r.value(&mean) - em).abs() < 1e-9,
        "mean {} vs {em}",
        r.value(&mean)
    );
    assert!(
        (r.value(&var) - ev).abs() < 1e-9,
        "var {} vs {ev}",
        r.value(&var)
    );
}

// ── tick-time parity ─────────────────────────────────────────────────────────

/// A time-weighted moment op ticks once per upstream tick (the crediting and
/// eviction are internal), so the emitted times are exactly the ticker's:
/// 0,100,200,300,400ns. Pin both the tick times and the paired mean values.
#[test]
fn time_weighted_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g)
        .cumulative_mean_time_weighted()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let series = r.value(&acc);
    let times: Vec<u64> = series.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![0, 100, 200, 300, 400], times);
    assert_series_approx(&values, &[1.0, 1.0, 1.5, 2.0, 2.5]);
}
