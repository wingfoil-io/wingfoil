#![cfg(feature = "statistics")]

//! Parity tests for the **time-weighted median** statistics
//! (`{cumulative,rolling,time_windowed}_median_time_weighted`), ported from the
//! legacy `adapters::statistics` `median(_, Weighting::Time)` path — the
//! recompute-per-tick `WindowStream::weighted_median`
//! (`legacy/wingfoil/src/adapters/statistics.rs`).
//!
//! Unlike the time-weighted *moments* (a West's-algorithm incremental
//! accumulator), the median has no cheap incremental form, so each tick recomputes
//! over the retained `(value, time)` window. Time-weighting semantics reproduced
//! (legacy `Weighting::Time`):
//!
//! * each retained sample is weighted by the interval it was in effect — the gap
//!   to its successor in the buffer, and the most recent sample by the gap to
//!   `now` (which is zero, since it was just pushed at `now`), so the newest
//!   sample carries no weight until the next tick advances the clock;
//! * zero-weight samples are dropped; if every retained sample has zero weight
//!   the latest value is returned;
//! * the samples are sorted by value and the result is the value at which
//!   cumulative weight first crosses half the total; an exact-boundary crossing
//!   averages the two straddling values (so unit-length intervals reproduce the
//!   even-count average, matching the count-weighted median);
//! * the leading edge uses only retained samples (count/time eviction first), so
//!   a value in effect before the window opened is not carried in.
//!
//! Ticks are 100ns apart. The legacy parity references are the unit tests
//! `rolling_median_time_weighted` (final 3.0, and the count median 3.5),
//! `rolling_median_over_time_window`, and `cumulative_median_over_all_samples`
//! in `legacy/wingfoil/src/adapters/statistics.rs`.

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

/// Direct (from-scratch) time-weighted median over `(value, time_ns)` samples
/// sorted by time, observed at `now_ns`: weight each by the gap to its successor
/// (the newest by the gap to `now`), drop zero weights, then take the value at
/// which cumulative weight crosses half the total (averaging the two straddling
/// values on an exact boundary). An independent transcription of the legacy
/// `WindowStream::weighted_median`, used as an oracle for the recompute path.
fn brute_time_weighted_median(samples: &[(f64, u64)], now_ns: u64) -> f64 {
    let n = samples.len();
    let mut pairs: Vec<(f64, f64)> = Vec::with_capacity(n);
    for i in 0..n {
        let (v, t) = samples[i];
        let next_t = if i + 1 < n { samples[i + 1].1 } else { now_ns };
        let w = (next_t - t) as f64;
        if w > 0.0 {
            pairs.push((v, w));
        }
    }
    if pairs.is_empty() {
        return samples.last().expect("non-empty samples").0;
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let half = pairs.iter().map(|&(_, w)| w).sum::<f64>() / 2.0;
    let mut cumulative = 0.0;
    for (i, &(v, w)) in pairs.iter().enumerate() {
        cumulative += w;
        if cumulative > half {
            return v;
        }
        if cumulative == half {
            return match pairs.get(i + 1) {
                Some(&(next, _)) => (v + next) / 2.0,
                None => v,
            };
        }
    }
    pairs.last().expect("non-empty pairs").0
}

// ── cumulative time-weighted median (unbounded window) ────────────────────────

/// Cumulative time-weighted median over 1..5 at 100ns ticks. Each earlier sample
/// is credited one 100ns interval; the newest carries zero weight. Walking the
/// weighted crossing gives 1, 1, 1.5, 2, 2.5 — the newest value never shifts the
/// median on arrival (it has no weight yet). Mirrors the legacy
/// `median(Window::Unbounded, Weighting::Time)`.
#[test]
fn cumulative_median_time_weighted_series() {
    let g = GraphBuilder::new();
    let med = counter_f64(&g)
        .cumulative_median_time_weighted()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&med), &[1.0, 1.0, 1.5, 2.0, 2.5]);
}

// ── count-windowed time-weighted median ───────────────────────────────────────

/// Mirrors legacy `rolling_median_time_weighted` (final 3.0), pinned as a full
/// series. Count window 4 over 1..5: the windows are {1},{1,2},{1,2,3},{1,2,3,4},
/// {2,3,4,5}; each retained sample except the newest is credited one 100ns
/// interval, so the newest is dropped. At the last tick the weights are
/// {2:100,3:100,4:100} (5 is "now", Δt 0) — cumulative crosses half (150) at 3,
/// so the weighted median is 3.0. The full series is 1, 1, 1.5, 2, 3.
#[test]
fn rolling_median_time_weighted_series() {
    let g = GraphBuilder::new();
    let med = counter_f64(&g).rolling_median_time_weighted(4).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&med), &[1.0, 1.0, 1.5, 2.0, 3.0]);
}

/// The time weighting must actually change the result versus the count-weighted
/// median: over the same {2,3,4,5} count window the count median is (3+4)/2 =
/// 3.5, whereas the time-weighted median is 3.0 (the newest sample 5 carries no
/// weight). Mirrors the two assertions in legacy `rolling_median_time_weighted`.
#[test]
fn rolling_median_time_vs_count_weighting_differ() {
    let g = GraphBuilder::new();
    let time = counter_f64(&g).rolling_median_time_weighted(4);
    let count = counter_f64(&g).rolling_median(4);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert!(
        (r.value(&time) - 3.0).abs() < 1e-9,
        "time-weighted {}",
        r.value(&time)
    );
    assert!(
        (r.value(&count) - 3.5).abs() < 1e-9,
        "count-weighted {}",
        r.value(&count)
    );
}

/// The recompute path must match a direct from-scratch weighted median over the
/// retained count window across a non-trivial sequence — i.e. eviction and
/// weighting have not drifted. Slide a window of 5 and compare the final value.
#[test]
fn rolling_median_time_weighted_matches_brute() {
    const N: u32 = 120;
    const W: usize = 5;
    let seq = |n: u64| ((n * 7) % 11) as f64 - 5.0;

    let g = GraphBuilder::new();
    let med = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .rolling_median_time_weighted(W);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // count() emits 1..=N at times 0,100,..,(N-1)*100; the count window retains
    // the last W samples; the observation time is the final tick, (N-1)*100.
    let now = (N as u64 - 1) * 100;
    let retained: Vec<(f64, u64)> = ((N as u64 - W as u64 + 1)..=N as u64)
        .map(|n| (seq(n), (n - 1) * 100))
        .collect();
    let expected = brute_time_weighted_median(&retained, now);
    assert!(
        (r.value(&med) - expected).abs() < 1e-9,
        "got {}, expected {expected}",
        r.value(&med)
    );
}

// ── time-windowed time-weighted median ────────────────────────────────────────

/// Time-weighted median over a 250ns window on 100ns ticks holds the last three
/// samples; each is credited its 100ns interval except the newest (Δt 0). From
/// the third tick the window has two equal-weight committed values straddling the
/// crossing, so the median averages them: 1, 1, 1.5, 2.5, 3.5 — at the last tick
/// the window is {3@200,4@300,5@400}, weights {3:100,4:100} → (3+4)/2 = 3.5.
/// Mirrors the legacy `median(Window::Time(_), Weighting::Time)`.
#[test]
fn time_windowed_median_time_weighted_series() {
    let g = GraphBuilder::new();
    let med = counter_f64(&g)
        .time_windowed_median_time_weighted(WIN)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&med), &[1.0, 1.0, 1.5, 2.5, 3.5]);
}

/// The recompute path must match a direct from-scratch weighted median over the
/// samples still inside a bounded time window across a non-trivial sequence.
#[test]
fn time_windowed_median_time_weighted_matches_brute() {
    const N: u32 = 90;
    // 350ns window over 100ns ticks retains ages 0,100,200,300 → four samples.
    const WIN_NS: u64 = 350;
    let win = Duration::from_nanos(WIN_NS);
    let seq = |n: u64| ((n * 5) % 13) as f64 - 6.0;

    let g = GraphBuilder::new();
    let med = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(move |n| seq(*n))
        .time_windowed_median_time_weighted(win);
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
    let expected = brute_time_weighted_median(&retained, now);
    assert!(
        (r.value(&med) - expected).abs() < 1e-9,
        "got {}, expected {expected}",
        r.value(&med)
    );
}

// ── cross-window and tick-time parity ─────────────────────────────────────────

/// A count window wide enough never to evict must equal the cumulative
/// (unbounded) time-weighted median tick-for-tick — the two share the same
/// recompute buffer, differing only in eviction, which a large window disables.
#[test]
fn wide_count_window_time_weighted_median_matches_cumulative() {
    let g = GraphBuilder::new();
    let rolling = counter_f64(&g)
        .rolling_median_time_weighted(1_000)
        .accumulate();
    let cumulative = counter_f64(&g)
        .cumulative_median_time_weighted()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&rolling), &r.value(&cumulative));
}

/// A constant stream's time-weighted median is that constant (all retained
/// samples share the value), and never `NaN`.
#[test]
fn cumulative_median_time_weighted_of_constant_is_constant() {
    let g = GraphBuilder::new();
    let med = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .cumulative_median_time_weighted()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    for v in r.value(&med) {
        assert!(!v.is_nan(), "median must not be NaN");
        assert!(
            (v - 7.0).abs() < 1e-10,
            "constant stream median should be 7.0, got {v}"
        );
    }
}

/// A time-weighted median op ticks once per upstream tick (the weighting and
/// eviction are internal), so the emitted times are exactly the ticker's:
/// 0,100,200,300,400ns. Pin both the tick times and the paired median values.
#[test]
fn time_weighted_median_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g)
        .cumulative_median_time_weighted()
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
