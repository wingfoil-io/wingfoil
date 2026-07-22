//! Parity tests for the count-windowed rolling statistics catalog
//! (`rolling_min` / `rolling_max` / `rolling_var` / `rolling_std` /
//! `rolling_median`), ported from the classic `adapters::statistics`
//! operators. Each test pins the emitted series **value-by-value** and, where
//! relevant, **tick-by-tick** against a hand-computed expected series derived
//! from the classic node semantics:
//!
//! * window clamped to `max(1)` (a zero window behaves as a window of one);
//! * `var`/`std` use the **sample** (ddof = 1) convention — divisor `n - 1`,
//!   and `0.0` until at least two samples are present (`std` clamps at zero so
//!   a constant window is `0.0`, not `NaN`);
//! * `median` averages the two middle values for an even-sized window;
//! * a rolling op ticks once per upstream tick (no seeding delay), so the
//!   output series has one entry per input tick.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::Stream;
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// 1, 2, 3, 4, 5 as `f64`, one tick every 100ns (ticks at 0,100,200,300,400ns).
fn counter_f64(g: &GraphBuilder) -> Stream<f64> {
    g.ticker(Duration::from_nanos(100))
        .count()
        .map(|i| *i as f64)
}

/// A non-monotonic `f64` sequence `((n * 7) % 13)` for `n = 1..`, so the
/// monotonic-deque min/max evict from both ends and the median actually sorts.
fn non_monotonic(g: &GraphBuilder) -> Stream<f64> {
    g.ticker(Duration::from_nanos(100))
        .count()
        .map(|n| ((*n * 7) % 13) as f64)
}

/// Assert two `f64` series equal element-wise within a tolerance.
fn assert_series_approx(got: &[f64], expected: &[f64]) {
    assert_eq!(got.len(), expected.len(), "length: {got:?} vs {expected:?}");
    for (i, (g, e)) in got.iter().zip(expected).enumerate() {
        assert!(
            (g - e).abs() < 1e-10,
            "at {i}: got {g}, expected {e} (series {got:?} vs {expected:?})"
        );
    }
}

// ── rolling_min / rolling_max ────────────────────────────────────────────────

/// Window 2 over 1,2,3,4,5. Min windows: {1},{1,2},{2,3},{3,4},{4,5} → 1,1,2,3,4;
/// max: 1,2,3,4,5.
#[test]
fn rolling_min_max_counter() {
    let g = GraphBuilder::new();
    let mn = counter_f64(&g).rolling_min(2).accumulate();
    let mx = counter_f64(&g).rolling_max(2).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 1.0, 2.0, 3.0, 4.0], r.value(&mn));
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 5.0], r.value(&mx));
}

/// Window 3 over the non-monotonic 7,1,8,2,9,3,10,4 — exercises deque eviction
/// from both ends. Windows:
/// {7},{7,1},{7,1,8},{1,8,2},{8,2,9},{2,9,3},{9,3,10},{3,10,4}.
#[test]
fn rolling_min_max_non_monotonic() {
    let g = GraphBuilder::new();
    let mn = non_monotonic(&g).rolling_min(3).accumulate();
    let mx = non_monotonic(&g).rolling_max(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    assert_eq!(vec![7.0, 1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0], r.value(&mn));
    assert_eq!(vec![7.0, 7.0, 8.0, 8.0, 9.0, 9.0, 10.0, 10.0], r.value(&mx));
}

// ── rolling_var / rolling_std ────────────────────────────────────────────────

/// Window 3 over 1,2,3,4,5 (sample variance, ddof = 1). Windows and vars:
/// {1}→0 (n<2), {1,2}→0.5, {1,2,3}→1, {2,3,4}→1, {3,4,5}→1. `std` is the
/// element-wise square root.
#[test]
fn rolling_var_std_counter() {
    let g = GraphBuilder::new();
    let var = counter_f64(&g).rolling_var(3).accumulate();
    let std = counter_f64(&g).rolling_std(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let expected_var = [0.0, 0.5, 1.0, 1.0, 1.0];
    assert_series_approx(&r.value(&var), &expected_var);
    let expected_std: Vec<f64> = expected_var.iter().map(|v| v.sqrt()).collect();
    assert_series_approx(&r.value(&std), &expected_std);
}

/// A constant window has zero variance; floating-point cancellation in the
/// incremental moments can make it a hair negative, so `std` must clamp at zero
/// rather than emit `NaN` (mirrors classic
/// `rolling_std_of_constant_window_is_zero_not_nan`).
#[test]
fn rolling_std_of_constant_window_is_zero_not_nan() {
    let g = GraphBuilder::new();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .rolling_std(3)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    for v in r.value(&std) {
        assert!(!v.is_nan(), "rolling_std must not be NaN");
        assert!(
            v.abs() < 1e-10,
            "constant window std should be 0.0, got {v}"
        );
    }
}

/// The incremental add/remove must not drift: slide a window across a long
/// non-trivial sequence and compare the final variance to a direct recompute.
#[test]
fn rolling_var_incremental_matches_direct_recompute() {
    const N: u32 = 200;
    const W: u64 = 10;
    let seq = |n: u64| ((n % 7) as f64) * 1.5 - 3.0;

    let g = GraphBuilder::new();
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .rolling_var(W as usize);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // `count()` emits 1..=N, so the final window is the last W values.
    let window: Vec<f64> = ((N as u64 - W + 1)..=N as u64).map(seq).collect();
    let mean = window.iter().sum::<f64>() / W as f64;
    let expected = window.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (W as f64 - 1.0);
    assert!((r.value(&var) - expected).abs() < 1e-9);
}

// ── rolling_median ───────────────────────────────────────────────────────────

/// Window 3 over 1,2,3,4,5. Windows and medians: {1}→1, {1,2}→1.5 (even),
/// {1,2,3}→2, {2,3,4}→3, {3,4,5}→4.
#[test]
fn rolling_median_counter() {
    let g = GraphBuilder::new();
    let med = counter_f64(&g).rolling_median(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 1.5, 2.0, 3.0, 4.0], r.value(&med));
}

/// Window 3 over the non-monotonic 7,1,8,2,9 — the median must sort each
/// window: {7}→7, {7,1}→4 (even), {7,1,8}→7, {1,8,2}→2, {8,2,9}→8.
#[test]
fn rolling_median_non_monotonic() {
    let g = GraphBuilder::new();
    let med = non_monotonic(&g).rolling_median(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![7.0, 4.0, 7.0, 2.0, 8.0], r.value(&med));
}

// ── window clamp ─────────────────────────────────────────────────────────────

/// A window of 0 clamps to `max(1)` (classic's `window.max(1)`), so every op
/// behaves as a window of one: each output is a function of just the latest
/// value (min = max = median = value; var = std = 0).
#[test]
fn rolling_window_zero_clamps_to_one() {
    let g = GraphBuilder::new();
    let mn = counter_f64(&g).rolling_min(0).accumulate();
    let mx = counter_f64(&g).rolling_max(0).accumulate();
    let var = counter_f64(&g).rolling_var(0).accumulate();
    let std = counter_f64(&g).rolling_std(0).accumulate();
    let med = counter_f64(&g).rolling_median(0).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let identity = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(identity, r.value(&mn));
    assert_eq!(identity, r.value(&mx));
    assert_eq!(identity, r.value(&med));
    assert_eq!(vec![0.0; 5], r.value(&var));
    assert_eq!(vec![0.0; 5], r.value(&std));
}

// ── tick-time parity ─────────────────────────────────────────────────────────

/// The rolling ops tick once per upstream tick with no seeding delay, so the
/// emitted times are exactly the ticker's: 0,100,200,300,400ns. Pin both the
/// tick times and the paired values for `rolling_min`.
#[test]
fn rolling_min_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g).rolling_min(2).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let series = r.value(&acc);
    let times: Vec<u64> = series.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![0, 100, 200, 300, 400], times);
    assert_eq!(vec![1.0, 1.0, 2.0, 3.0, 4.0], values);
}
