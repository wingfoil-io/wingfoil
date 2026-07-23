//! Parity tests for the **time-windowed** rolling statistics
//! (`time_windowed_sum` / `mean` / `min` / `max` / `var` / `std` / `median`),
//! ported from the classic `adapters::statistics` operators over
//! `Window::Time(_)` with `Weighting::Count` (the classic `WindowStream` /
//! `RollingMomentStream` time-window path, `wingfoil/src/adapters/statistics.rs`).
//!
//! Eviction / boundary semantics reproduced (classic `Window::Time`):
//!
//! * on each tick the current sample is pushed at `now = ctx.time()`, then every
//!   entry whose age `now - t` is **strictly greater** than the window is
//!   evicted — so an entry exactly `window` old is **retained** (an inclusive
//!   trailing boundary);
//! * the current sample has age 0, so it is always in window and the window is
//!   **never empty** — classic has no empty-window instant, and even a
//!   zero-width window keeps the current sample;
//! * `var`/`std` use the sample (ddof = 1) convention — `0.0` until at least two
//!   samples are in the window (`std` clamps at zero, never `NaN`);
//! * `median` averages the two middle values for an even count.
//!
//! Ticks are 100ns apart, so a 250ns window retains the three most recent
//! samples (ages 0, 100, 200) and drops the rest — mirroring the classic
//! `WIN = 250ns` fixtures. The counter fixture `1,2,3,4,5` therefore ends on the
//! window `{3,4,5}`, and the tests pin the **whole** emitted series so the
//! mid-stream evictions (the value 1 ageing out at t = 300ns, then 2 at
//! t = 400ns) are exercised, not just the final window.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::Stream;
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

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

// ── time_windowed_sum ────────────────────────────────────────────────────────

/// Windows retained per tick (250ns): {1},{1,2},{1,2,3},{2,3,4},{3,4,5}. The
/// value 1 ages out at t=300ns and 2 at t=400ns, so the sums are
/// 1,3,6,9,12 — the eviction is doing real work (a cumulative sum would give
/// 1,3,6,10,15).
#[test]
fn time_windowed_sum_counter() {
    let g = GraphBuilder::new();
    let s = counter_f64(&g).time_windowed_sum(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 3.0, 6.0, 9.0, 12.0], r.value(&s));
}

// ── time_windowed_mean ───────────────────────────────────────────────────────

/// Same windows: means 1, 1.5, 2, 3, 4. The final `{3,4,5}` mean is 4.0
/// (mirrors classic `rolling_mean_over_time_window_count_and_time`, count case).
#[test]
fn time_windowed_mean_counter() {
    let g = GraphBuilder::new();
    let m = counter_f64(&g).time_windowed_mean(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&m), &[1.0, 1.5, 2.0, 3.0, 4.0]);
}

// ── time_windowed_min / time_windowed_max ────────────────────────────────────

/// Windows {1},{1,2},{1,2,3},{2,3,4},{3,4,5}: min rises 1,1,1,2,3 as the small
/// values age out (a cumulative min would stay 1), max is 1,2,3,4,5. Final
/// {3,4,5}: min 3, max 5 (mirrors classic `rolling_min_max_over_time_window`).
#[test]
fn time_windowed_min_max_counter() {
    let g = GraphBuilder::new();
    let mn = counter_f64(&g).time_windowed_min(WIN).accumulate();
    let mx = counter_f64(&g).time_windowed_max(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 1.0, 1.0, 2.0, 3.0], r.value(&mn));
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 5.0], r.value(&mx));
}

/// Min/max over a non-monotonic sequence in a 250ns window, checked every tick
/// against a brute-force scan of the retained window — exercises the monotonic
/// deque evicting from both ends (stale front by age, dominated back on push).
#[test]
fn time_windowed_min_max_non_monotonic_matches_brute_force() {
    const N: u32 = 40;
    let seq = |n: u64| ((n * 7) % 13) as f64;

    let g = GraphBuilder::new();
    let mn = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .time_windowed_min(WIN)
        .accumulate();
    let mx = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .time_windowed_max(WIN)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    let got_min = r.value(&mn);
    let got_max = r.value(&mx);
    // After tick k (0-based) the stream has emitted seq(1..=k+1) at times
    // 0,100,..,k*100; the 250ns window retains those within 250ns of now = k*100,
    // i.e. ages 0,100,200 → the last three (or fewer near the start).
    for k in 0..got_min.len() {
        let n = (k + 1) as u64;
        let start = if n > 3 { n - 2 } else { 1 };
        let window: Vec<f64> = (start..=n).map(seq).collect();
        let emin = window.iter().copied().fold(f64::INFINITY, f64::min);
        let emax = window.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert_eq!(got_min[k], emin, "min mismatch at tick {k}");
        assert_eq!(got_max[k], emax, "max mismatch at tick {k}");
    }
}

// ── time_windowed_var / time_windowed_std ────────────────────────────────────

/// Windows {1},{1,2},{1,2,3},{2,3,4},{3,4,5} sample variance (ddof = 1):
/// 0 (n<2), 0.5, 1, 1, 1. Final `{3,4,5}` var 1.0 (mirrors classic
/// `rolling_var_over_time_window_count`). `std` is the element-wise root.
#[test]
fn time_windowed_var_std_counter() {
    let g = GraphBuilder::new();
    let var = counter_f64(&g).time_windowed_var(WIN).accumulate();
    let std = counter_f64(&g).time_windowed_std(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let expected_var = [0.0, 0.5, 1.0, 1.0, 1.0];
    assert_series_approx(&r.value(&var), &expected_var);
    let expected_std: Vec<f64> = expected_var.iter().map(|v| v.sqrt()).collect();
    assert_series_approx(&r.value(&std), &expected_std);
}

/// The incremental add/remove moments must match a direct recompute over the
/// retained window (i.e. eviction has not drifted): slide a 350ns window over a
/// non-trivial sequence and compare the final mean/variance to a from-scratch
/// computation over exactly the samples still in the window.
#[test]
fn time_windowed_moments_match_direct_recompute() {
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
        .time_windowed_mean(win);
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .time_windowed_var(win);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // count() emits 1..=N at times 0,100,..,(N-1)*100; final now = (N-1)*100.
    // Retained = samples within WIN_NS of now (age <= WIN_NS).
    let now = (N as u64 - 1) * 100;
    let retained: Vec<f64> = (1..=N as u64)
        .filter(|n| now - (n - 1) * 100 <= WIN_NS)
        .map(seq)
        .collect();
    let em = retained.iter().sum::<f64>() / retained.len() as f64;
    let ev = retained.iter().map(|v| (v - em).powi(2)).sum::<f64>() / (retained.len() as f64 - 1.0);
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

/// A constant stream over a time window has zero variance; `std` must clamp at
/// zero (revert-scheme drift can make the incremental variance a hair negative)
/// rather than emit `NaN`.
#[test]
fn time_windowed_std_of_constant_is_zero_not_nan() {
    let g = GraphBuilder::new();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .time_windowed_std(WIN)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    for v in r.value(&std) {
        assert!(!v.is_nan(), "std must not be NaN");
        assert!(
            v.abs() < 1e-10,
            "constant window std should be 0.0, got {v}"
        );
    }
}

// ── time_windowed_median ─────────────────────────────────────────────────────

/// Windows {1},{1,2},{1,2,3},{2,3,4},{3,4,5}: medians 1, 1.5 (even), 2, 3, 4.
/// Final {3,4,5} median 4.0 (mirrors classic `rolling_median_over_time_window`).
#[test]
fn time_windowed_median_counter() {
    let g = GraphBuilder::new();
    let med = counter_f64(&g).time_windowed_median(WIN).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&med), &[1.0, 1.5, 2.0, 3.0, 4.0]);
}

// ── zero-width window (degenerate, never empty) ──────────────────────────────

/// Classic never produces an empty window (the current sample has age 0 and is
/// always retained). A zero-width window is the degenerate limit: every entry
/// except the current one (age > 0) is evicted, so each stat is a function of
/// just the latest value — sum = mean = min = max = median = value, var = std =
/// 0. This pins the closest-to-empty behaviour classic allows.
#[test]
fn time_windowed_zero_width_keeps_only_current() {
    let g = GraphBuilder::new();
    let zero = Duration::ZERO;
    let s = counter_f64(&g).time_windowed_sum(zero).accumulate();
    let m = counter_f64(&g).time_windowed_mean(zero).accumulate();
    let mn = counter_f64(&g).time_windowed_min(zero).accumulate();
    let mx = counter_f64(&g).time_windowed_max(zero).accumulate();
    let var = counter_f64(&g).time_windowed_var(zero).accumulate();
    let med = counter_f64(&g).time_windowed_median(zero).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let identity = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(identity, r.value(&s));
    assert_eq!(identity, r.value(&m));
    assert_eq!(identity, r.value(&mn));
    assert_eq!(identity, r.value(&mx));
    assert_eq!(identity, r.value(&med));
    assert_eq!(vec![0.0; 5], r.value(&var));
}

// ── tick-time parity ─────────────────────────────────────────────────────────

/// A time-windowed op ticks once per upstream tick (eviction is internal), so
/// the emitted times are exactly the ticker's: 0,100,200,300,400ns. Pin both
/// the tick times and the paired `time_windowed_min` values.
#[test]
fn time_windowed_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g)
        .time_windowed_min(WIN)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let series = r.value(&acc);
    let times: Vec<u64> = series.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![0, 100, 200, 300, 400], times);
    assert_eq!(vec![1.0, 1.0, 1.0, 2.0, 3.0], values);
}
