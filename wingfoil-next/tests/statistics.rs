//! Phase 4 adapter porting, proof of tractability: the statistics adapter's
//! EWMA operator ported as an `Op` — stateful, clock-aware (half-life decay
//! off engine time), seeded with an explicit init flag. Mirrors the classic
//! `adapters::statistics` EWMA unit tests exactly.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

fn counter_f64(g: &GraphBuilder) -> wingfoil_next::fluent::Stream<f64> {
    g.ticker(Duration::from_nanos(100))
        .count()
        .map(|i| *i as f64)
}

/// Mirrors `ewma_of_sequence`: count 1,2,3,4 with alpha 0.5, seeded on the
/// first sample → e1=1, e2=1.5, e3=2.25, e4=3.125.
#[test]
fn ewma_of_sequence() {
    let g = GraphBuilder::new();
    let ewma = counter_f64(&g).ewma_per_tick(0.5);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert!((r.value(&ewma) - 3.125).abs() < 1e-10);
}

/// Mirrors `ewma_seeds_on_first_sample`: a constant stream of 5 stays 5.0.
#[test]
fn ewma_seeds_on_first_sample() {
    let g = GraphBuilder::new();
    let ewma = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 5.0)
        .ewma_per_tick(0.3);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert!((r.value(&ewma) - 5.0).abs() < 1e-10);
}

/// Mirrors `ewma_does_not_reset_at_zero`: inputs 0,0,5 with alpha 0.5 seed to
/// 0 and decay (0 → 0 → 2.5) rather than re-seeding on the 5.
#[test]
fn ewma_does_not_reset_at_zero() {
    let g = GraphBuilder::new();
    let ewma = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|n| if *n <= 2 { 0.0 } else { 5.0 })
        .ewma_per_tick(0.5);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert!((r.value(&ewma) - 2.5).abs() < 1e-10);
}

/// Rolling mean over a sliding window — mirrors classic
/// `rolling_mean_over_window`. Window 3 over 1,2,3,4,5: 1, 1.5, 2, 3, 4.
#[test]
fn rolling_mean_over_window() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g).rolling_mean(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 1.5, 2.0, 3.0, 4.0], r.value(&acc));
}

/// Rolling sum over a sliding window — mirrors classic
/// `rolling_sum_over_window`. Window 3 over 1,2,3,4,5: 1, 3, 6, 9, 12.
#[test]
fn rolling_sum_over_window() {
    let g = GraphBuilder::new();
    let acc = counter_f64(&g).rolling_sum(3).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 3.0, 6.0, 9.0, 12.0], r.value(&acc));
}

/// Half-life decay of a constant stream stays at the seed (mirrors
/// `ewma_decay_constant_stream_is_constant`) — exercises the clock-driven
/// decay path.
#[test]
fn ewma_half_life_of_constant_is_constant() {
    let g = GraphBuilder::new();
    let ewma = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|_| 7.0)
        .ewma_half_life(Duration::from_nanos(50));
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert!((r.value(&ewma) - 7.0).abs() < 1e-10);
}

/// Real decay over a *varying* stream — mirrors classic
/// `ewma_decay_matches_per_tick_when_dt_equals_half_life`. With Δt (100ns tick)
/// equal to the half-life, alpha = 1 - 2^-1 = 0.5 every tick, so over 1,2,3,4
/// the half-life EWMA matches `ewma_per_tick(0.5)`: e1=1, e2=1.5, e3=2.25,
/// e4=3.125. Unlike the constant-stream case above, this pins the actual decay
/// math (a constant stream stays at its seed for *any* alpha).
#[test]
fn ewma_half_life_matches_per_tick_when_dt_equals_half_life() {
    let g = GraphBuilder::new();
    let ewma = counter_f64(&g).ewma_half_life(Duration::from_nanos(100));
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert!((r.value(&ewma) - 3.125).abs() < 1e-10);
}

/// A window of 0 is meaningless and clamps to `max(1)` (mirrors classic's
/// `Window::Count(n.max(1))` / `window.max(1)` clamp), so `rolling_sum(0)` and
/// `rolling_mean(0)` behave as a window of 1: each output is just the latest
/// value.
#[test]
fn rolling_window_zero_clamps_to_one() {
    let g = GraphBuilder::new();
    let sum = counter_f64(&g).rolling_sum(0).accumulate();
    let mean = counter_f64(&g).rolling_mean(0).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 5.0], r.value(&sum));
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 5.0], r.value(&mean));
}
