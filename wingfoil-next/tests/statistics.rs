//! Phase 4 adapter porting, proof of tractability: the statistics adapter's
//! EWMA operator ported as an `Op` — stateful, clock-aware (half-life decay
//! off engine time), seeded with an explicit init flag. Mirrors the classic
//! `adapters::statistics` EWMA unit tests exactly.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;

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
