//! Parity tests for the cumulative (unbounded-window) statistics catalog
//! (`cumulative_sum` / `cumulative_mean` / `cumulative_min` / `cumulative_max`
//! / `cumulative_var` / `cumulative_std` / `cumulative_median`), ported from
//! the classic `adapters::statistics` operators over `Window::Unbounded` with
//! `Weighting::Count`. Each test pins the emitted series **value-by-value**
//! (and, where relevant, **tick-by-tick**) against a hand-computed expected
//! series derived from the classic node semantics:
//!
//! * mean/variance/std use incrementally maintained moments (Welford's online
//!   algorithm), so they are numerically faithful — not a naive
//!   sum-of-squares;
//! * `var`/`std` use the **sample** (ddof = 1) convention — divisor `n - 1`,
//!   and `0.0` until at least two samples are present (`std` clamps at zero so
//!   a constant stream is `0.0`, not `NaN`);
//! * `median` averages the two middle values for an even count;
//! * a cumulative op ticks once per upstream tick (no seeding delay), so the
//!   output series has one entry per input tick.
//!
//! The classic parity references are the unit tests `cumulative_sum_min_max`,
//! `cumulative_median_over_all_samples`, `mean_count_is_arithmetic_mean`, and
//! `variance_count_is_sample_variance` in `wingfoil/src/adapters/statistics.rs`.

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

/// A fixed non-trivial sequence emitted one value per tick: the `n`-th tick
/// (1-based, from `count()`) yields `SEQUENCE[n - 1]`. Used for the hand-checked
/// variance/median cases so the assertions pin the *decay math*, not a value
/// that happens to hold for any input (as a constant stream would).
const SEQUENCE: [f64; 8] = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];

fn sequence(g: &GraphBuilder) -> Stream<f64> {
    g.ticker(Duration::from_nanos(100))
        .count()
        .map(|n| SEQUENCE[(*n - 1) as usize])
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

// ── cumulative_sum / cumulative_min / cumulative_max ─────────────────────────

/// Mirrors classic `cumulative_sum_min_max`. Over 1,2,3,4,5 the running sum is
/// 1,3,6,10,15; the running min stays 1; the running max climbs 1,2,3,4,5.
/// The min case feeds a *descending* stream (5,4,3,2,1) so the running minimum
/// must keep falling — a constant or ascending stream would pass trivially.
#[test]
fn cumulative_sum_min_max() {
    let g = GraphBuilder::new();
    let sum = counter_f64(&g).cumulative_sum().accumulate();
    let mx = counter_f64(&g).cumulative_max().accumulate();
    // 6 - n over 1..=5 → 5,4,3,2,1, so the running min falls 5,4,3,2,1.
    let mn = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|n| (6 - *n) as f64)
        .cumulative_min()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 3.0, 6.0, 10.0, 15.0], r.value(&sum));
    assert_eq!(vec![5.0, 4.0, 3.0, 2.0, 1.0], r.value(&mn));
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 5.0], r.value(&mx));
}

/// The running max over an ascending stream climbs; the running min over the
/// same ascending stream stays pinned at the first value — a direct check that
/// the `Option`-seeded extreme does not fold in the `0.0` default.
#[test]
fn cumulative_min_pins_first_on_ascending() {
    let g = GraphBuilder::new();
    let mn = counter_f64(&g).cumulative_min().accumulate();
    let mx = counter_f64(&g).cumulative_max().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 1.0, 1.0, 1.0, 1.0], r.value(&mn));
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 5.0], r.value(&mx));
}

// ── cumulative_mean ──────────────────────────────────────────────────────────

/// Mirrors classic `mean_count_is_arithmetic_mean` (final value 3.0), pinned as
/// a full series: the expanding arithmetic mean of 1,2,3,4,5 is
/// 1, 1.5, 2, 2.5, 3.
#[test]
fn cumulative_mean_is_expanding_arithmetic_mean() {
    let g = GraphBuilder::new();
    let mean = counter_f64(&g).cumulative_mean().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&mean), &[1.0, 1.5, 2.0, 2.5, 3.0]);
}

// ── cumulative_var / cumulative_std ──────────────────────────────────────────

/// Mirrors classic `variance_count_is_sample_variance` (final value 2.5 over
/// 1,2,3,4,5), pinned as a full series. Sample variance (ddof = 1) of the first
/// `k` of 1..5:
///   {1}→0 (n<2), {1,2}→0.5, {1,2,3}→1, {1,2,3,4}→5/3, {1,2,3,4,5}→2.5.
/// `std` is the element-wise square root.
#[test]
fn cumulative_var_std_counter() {
    let g = GraphBuilder::new();
    let var = counter_f64(&g).cumulative_var().accumulate();
    let std = counter_f64(&g).cumulative_std().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let expected_var = [0.0, 0.5, 1.0, 5.0 / 3.0, 2.5];
    assert_series_approx(&r.value(&var), &expected_var);
    let expected_std: Vec<f64> = expected_var.iter().map(|v| v.sqrt()).collect();
    assert_series_approx(&r.value(&std), &expected_std);
}

/// A real-variance numeric case on the fixed sequence 2,4,4,4,5,5,7,9 (the
/// textbook example whose *population* std is exactly 2). The final cumulative
/// sample variance is a hand-computed known value: mean = 40/8 = 5, sum of
/// squared deviations = 9+1+1+1+0+0+4+16 = 32, so sample variance = 32/7 and
/// sample std = sqrt(32/7). Unlike a constant stream (variance 0 for any math),
/// this pins the actual moment computation.
#[test]
fn cumulative_var_std_real_sequence_hand_computed() {
    let g = GraphBuilder::new();
    let var = sequence(&g).cumulative_var();
    let std = sequence(&g).cumulative_std();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(SEQUENCE.len() as u32))
        .unwrap();
    let expected_var = 32.0 / 7.0;
    assert!(
        (r.value(&var) - expected_var).abs() < 1e-10,
        "var {} vs {expected_var}",
        r.value(&var)
    );
    assert!(
        (r.value(&std) - expected_var.sqrt()).abs() < 1e-10,
        "std {} vs {}",
        r.value(&std),
        expected_var.sqrt()
    );
}

/// The incremental Welford moments must match a direct recompute over all
/// history: run a long non-trivial sequence and compare the final cumulative
/// mean/variance to a from-scratch two-pass computation over every sample.
#[test]
fn cumulative_moments_match_direct_recompute() {
    const N: u32 = 200;
    let seq = |n: u64| ((n % 7) as f64) * 1.5 - 3.0;

    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .cumulative_mean();
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .cumulative_var();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // `count()` emits 1..=N, so all history is seq(1..=N).
    let all: Vec<f64> = (1..=N as u64).map(seq).collect();
    let expected_mean = all.iter().sum::<f64>() / all.len() as f64;
    let expected_var =
        all.iter().map(|v| (v - expected_mean).powi(2)).sum::<f64>() / (all.len() as f64 - 1.0);
    assert!((r.value(&mean) - expected_mean).abs() < 1e-9);
    assert!((r.value(&var) - expected_var).abs() < 1e-9);
}

/// A constant stream has zero cumulative variance; floating-point cancellation
/// can make it a hair negative, so `std` must clamp at zero rather than emit
/// `NaN` (mirrors the classic constant-window std guard).
#[test]
fn cumulative_std_of_constant_is_zero_not_nan() {
    let g = GraphBuilder::new();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .cumulative_std()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    for v in r.value(&std) {
        assert!(!v.is_nan(), "cumulative_std must not be NaN");
        assert!(
            v.abs() < 1e-10,
            "constant stream std should be 0.0, got {v}"
        );
    }
}

/// Sample variance is defined as `0.0` (not NaN) with a single sample.
#[test]
fn cumulative_var_is_zero_with_single_sample() {
    let g = GraphBuilder::new();
    let var = counter_f64(&g).cumulative_var();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    assert_eq!(0.0, r.value(&var));
}

// ── cumulative_median ────────────────────────────────────────────────────────

/// Mirrors classic `cumulative_median_over_all_samples` (final value 3.0 over
/// 1,2,3,4,5), pinned as a full series. The median of the first `k` of 1..5:
/// {1}→1, {1,2}→1.5 (even), {1,2,3}→2, {1,2,3,4}→2.5 (even), {1,2,3,4,5}→3.
#[test]
fn cumulative_median_counter() {
    let g = GraphBuilder::new();
    let med = counter_f64(&g).cumulative_median().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&med), &[1.0, 1.5, 2.0, 2.5, 3.0]);
}

/// Median must sort *all* retained history, not just recent samples. Over the
/// non-monotonic 2,4,4,4,5,5,7,9 the cumulative median walks the sorted middle:
///   {2}→2, {2,4}→3, {2,4,4}→4, {2,4,4,4}→4, {2,4,4,4,5}→4,
///   {2,4,4,4,5,5}→4, {2,4,4,4,5,5,7}→4, {2,4,4,4,5,5,7,9}→(4+5)/2=4.5.
#[test]
fn cumulative_median_non_monotonic() {
    let g = GraphBuilder::new();
    let med = sequence(&g).cumulative_median().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(SEQUENCE.len() as u32))
        .unwrap();
    assert_series_approx(&r.value(&med), &[2.0, 3.0, 4.0, 4.0, 4.0, 4.0, 4.0, 4.5]);
}

// ── tick-time parity ─────────────────────────────────────────────────────────

/// The cumulative ops tick once per upstream tick with no seeding delay, so the
/// emitted times are exactly the ticker's: 0,100,200,300,400ns. Pin both the
/// tick times and the paired values for `cumulative_sum`.
#[test]
fn cumulative_sum_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g).cumulative_sum().with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let series = r.value(&acc);
    let times: Vec<u64> = series.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![0, 100, 200, 300, 400], times);
    assert_eq!(vec![1.0, 3.0, 6.0, 10.0, 15.0], values);
}
