//! Parity tests for the cumulative **time-weighted** statistics
//! (`time_weighted_mean` / `time_weighted_var` / `time_weighted_std`), ported
//! from the classic `adapters::statistics` operators over `Window::Unbounded`
//! with `Weighting::Time` (the `WeightedMoments` / West's weighted-Welford path
//! driven by `MomentStream`).
//!
//! Exact weighting semantics reproduced (classic `MomentStream` under
//! `Weighting::Time`, `wingfoil/src/adapters/statistics.rs` ~329):
//!
//! * each sample is weighted by how long it was **in effect** — on every tick
//!   the *previous* value is credited with the elapsed Δt (from the graph
//!   clock) before the new value takes over (a left-continuous step signal);
//! * the most recent sample therefore contributes nothing until the next tick
//!   advances the clock (so with N ticks only N-1 samples are ever weighted);
//! * the **first** sample seeds the tracker with no weight — the emitted mean
//!   is the raw sample (not `NaN`), and the variance is `0.0`;
//! * a zero-Δt (simultaneous) tick adds no weight;
//! * variance is the **population** form `m2 / Σ Δt` (time weighting has no
//!   clean ddof correction) — not the sample (ddof = 1) form the count-weighted
//!   operators use.
//!
//! Each test pins the emitted series (and tick times) against hand-computed
//! expected time-weighted moments on a known `(value, Δt)` sequence with
//! unambiguous timestamps, and includes a case where uneven Δt changes the
//! result versus the count-weighted mean (proving the weighting is real).

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::Stream;
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A stream that ticks at **uneven** spacings: on a 100ns grid (count 1..5 at
/// t = 0,100,200,300,400ns) the tick at t = 200ns (n = 3) is dropped, so the
/// values 1,2,3,4 land at t = 0,100,300,400ns. The gaps between consecutive
/// ticks are therefore 100, 200, 100 ns — uneven, so time weighting differs
/// from count weighting.
///
/// `map_filter` renumbers around the dropped tick: n=1→1, n=2→2, (n=3 dropped),
/// n=4→3, n=5→4.
fn uneven(g: &GraphBuilder) -> Stream<f64> {
    g.ticker(Duration::from_nanos(100)).count().map_filter(|n| {
        let emit = *n != 3;
        let value = (if *n < 3 { *n } else { *n - 1 }) as f64;
        (value, emit)
    })
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

// ── time_weighted_mean ───────────────────────────────────────────────────────

/// Uneven grid: values 1,2,3,4 in effect for Δt = _, 100, 200, 100 ns (the
/// first is never credited a preceding interval; the last is not yet closed).
/// After each tick the previous value is credited:
///   t0   : seed → mean = 1                    (empty)
///   t100 : credit {1:100}            → mean = 1
///   t300 : credit {1:100, 2:200}     → mean = (100 + 400)/300 = 1.6667
///   t400 : credit {1:100, 2:200, 3:100} → mean = (100 + 400 + 300)/400 = 2.0
/// The final 2.0 differs from the count-weighted cumulative mean of 1,2,3,4
/// (which is 2.5) — the uneven Δt is doing real work.
#[test]
fn time_weighted_mean_uneven_intervals() {
    let g = GraphBuilder::new();
    let twm = uneven(&g).time_weighted_mean().accumulate();
    // Count-weighted cumulative mean of the same values, for contrast.
    let cwm = uneven(&g)
        .fold((0.0, 0.0), |acc: &mut (f64, f64), v: &f64| {
            acc.0 += *v;
            acc.1 += 1.0;
        })
        .map(|(sum, n)| if *n > 0.0 { sum / n } else { 0.0 })
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_series_approx(&r.value(&twm), &[1.0, 1.0, 5.0 / 3.0, 2.0]);
    // The count-weighted mean over the same values is different (2.5 vs 2.0).
    assert_series_approx(&r.value(&cwm), &[1.0, 1.5, 2.0, 2.5]);
    assert!(
        (r.value(&twm).last().unwrap() - r.value(&cwm).last().unwrap()).abs() > 0.4,
        "time weighting must diverge from count weighting on uneven Δt"
    );
}

/// Mirrors the classic `mean_time_weighted_lags_by_one_interval`: with *evenly*
/// spaced ticks (100ns), each of 1,2,3,4 is in effect one interval before the
/// next and 5 has not yet been credited, so TWA = (1+2+3+4)/4 = 2.5 (versus the
/// count mean 3.0).
#[test]
fn time_weighted_mean_even_intervals_lags_by_one() {
    let g = GraphBuilder::new();
    let twm = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|n| *n as f64)
        .time_weighted_mean();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert!((r.value(&twm) - 2.5).abs() < 1e-9, "got {}", r.value(&twm));
}

// ── time_weighted_var / time_weighted_std ────────────────────────────────────

/// Uneven grid (values 1,2,3,4; Δt credited 100,200,100). Time-weighted
/// population variance `m2 / Σ Δt`:
///   t0   : 0                                   (empty)
///   t100 : {1:100}          mean 1     → var 0
///   t300 : {1:100, 2:200}   mean 5/3   → m2 = 100·(1-5/3)² + 200·(2-5/3)²
///           = 100·(4/9) + 200·(1/9) = 600/9; var = (600/9)/300 = 2/9 ≈ 0.2222
///   t400 : {1:100, 2:200, 3:100} mean 2 → m2 = 100·1 + 200·0 + 100·1 = 200;
///           var = 200/400 = 0.5
/// `std` is the element-wise square root.
#[test]
fn time_weighted_var_std_uneven_intervals() {
    let g = GraphBuilder::new();
    let var = uneven(&g).time_weighted_var().accumulate();
    let std = uneven(&g).time_weighted_std().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let expected_var = [0.0, 0.0, 2.0 / 9.0, 0.5];
    assert_series_approx(&r.value(&var), &expected_var);
    let expected_std: Vec<f64> = expected_var.iter().map(|v| v.sqrt()).collect();
    assert_series_approx(&r.value(&std), &expected_std);
}

/// Mirrors the classic `variance_time_weighted_is_population_over_weight`: with
/// evenly spaced ticks the credited values {1,2,3,4} each carry weight 100, so
/// mean = 2.5, m2 = 100·(2.25+0.25+0.25+2.25) = 500, and the population variance
/// is 500 / 400 = 1.25.
#[test]
fn time_weighted_var_even_intervals_population() {
    let g = GraphBuilder::new();
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|n| *n as f64)
        .time_weighted_var();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert!((r.value(&var) - 1.25).abs() < 1e-9, "got {}", r.value(&var));
}

// ── first-sample / single-sample edge cases ──────────────────────────────────

/// The first sample seeds the tracker with no weight: the mean is the raw
/// sample and the variance is `0.0` (not `NaN`).
#[test]
fn time_weighted_first_sample_seeds() {
    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .time_weighted_mean();
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .time_weighted_var();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .time_weighted_std();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    assert_eq!(7.0, r.value(&mean));
    assert_eq!(0.0, r.value(&var));
    assert_eq!(0.0, r.value(&std));
}

/// A constant stream is time-weighted to the same constant, with zero variance
/// (and `std` must clamp at zero, not emit `NaN`), regardless of the (even)
/// spacing.
#[test]
fn time_weighted_constant_is_constant() {
    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 3.5)
        .time_weighted_mean()
        .accumulate();
    let std = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 3.5)
        .time_weighted_std()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    for v in r.value(&mean) {
        assert!((v - 3.5).abs() < 1e-10, "mean should stay 3.5, got {v}");
    }
    for v in r.value(&std) {
        assert!(!v.is_nan(), "std must not be NaN");
        assert!(v.abs() < 1e-10, "constant std should be 0.0, got {v}");
    }
}

// ── direct-recompute cross-check ─────────────────────────────────────────────

/// Slide over a long irregularly-gated sequence and compare the incrementally
/// maintained (West) time-weighted mean/variance to an independent **two-pass**
/// recompute over the exact `(value, Δt)` contributions — i.e. West has not
/// drifted from `Σwx/Σw` and `Σw(x-mean)²/Σw`.
#[test]
fn time_weighted_moments_match_direct_recompute() {
    const N: u32 = 60;
    // Drop every 4th tick, so the surviving ticks are unevenly spaced.
    let dropped = |n: u64| n.is_multiple_of(4);
    let seq = |n: u64| ((n * 7) % 13) as f64;

    let g = GraphBuilder::new();
    let mean = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map_filter(move |n| (seq(*n), !dropped(*n)))
        .time_weighted_mean();
    let var = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map_filter(move |n| (seq(*n), !dropped(*n)))
        .time_weighted_var();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(N)).unwrap();

    // Reconstruct the fired ticks: value seq(n) at time (n-1)*100ns for every
    // n in 1..=N that was not dropped. Each fired tick closes the previous
    // fired value's interval, crediting it with the gap between the two times.
    let fired: Vec<(u64, f64)> = (1..=N as u64)
        .filter(|n| !dropped(*n))
        .map(|n| ((n - 1) * 100, seq(n)))
        .collect();
    let mut contribs: Vec<(f64, f64)> = Vec::new(); // (value, weight = Δt)
    for w in fired.windows(2) {
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

/// The time-weighted ops tick once per upstream tick (the dropped tick does not
/// fire), so the emitted times are exactly the surviving ticks: 0,100,300,400ns.
/// Pin both the tick times and the paired time-weighted mean.
#[test]
fn time_weighted_tick_times_match_upstream() {
    let g = GraphBuilder::new();
    let acc = uneven(&g).time_weighted_mean().with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    let series = r.value(&acc);
    let times: Vec<u64> = series.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![0, 100, 300, 400], times);
    assert_series_approx(&values, &[1.0, 1.0, 5.0 / 3.0, 2.0]);
}
