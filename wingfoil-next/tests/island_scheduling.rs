//! Islands containing the combinators the existing nested-island suite does
//! not exercise: `constant` + `merge` + `filter` in one island, and a
//! statistics op (`ewma_per_tick`) in another. Closes the fable-review blind
//! spot "no island containing merge/filter/constant/statistics".
//!
//! Each test pins interpreted == compiled == nested-in-interpreted, and (for
//! the value-carrying islands) cross-checks the nested expansion against the
//! identical wiring done flat in the same graph, so all execution paths run
//! under the same kernel on the same cycles and must agree exactly.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;
use wingfoil_next::stats::StatisticsOps;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

wingfoil_next::graph! {
    // `constant` (a one-shot source), `filter` (a conditional gate), and
    // `merge` (a tie-broken join) all inside one island.
    fn merge_filter_constant(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let n = g.ticker(Duration::from_nanos(10)).count();
        let keep_even = n.map(|i| i.is_multiple_of(2));
        let evens = n.filter(&keep_even);
        let seed = g.constant(9u64);
        let merged = seed.merge(&evens).accumulate();
        merged
    }
}

/// The island emits the constant `9` on cycle 1 (only the constant ticks
/// there — `n=1` is odd, so the filter stays shut), then the evens `2, 4, 6`
/// as they pass the filter: `[9, 2, 4, 6]` over six cycles.
#[test]
fn island_merge_filter_constant_all_paths() {
    let run_for = RunFor::Cycles(6);

    let (mut runner, out) = merge_filter_constant::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);
    assert_eq!(vec![9, 2, 4, 6], interpreted);

    let (compiled,) = merge_filter_constant::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled, "interpreted == compiled");

    // Nested inside an interpreted graph, cross-checked against identical flat
    // wiring driven by the same kernel.
    let g = GraphBuilder::new();
    let island = merge_filter_constant::nested(&g);
    let flat = {
        let n = g.ticker(Duration::from_nanos(10)).count();
        let keep_even = n.map(|i| i.is_multiple_of(2));
        let evens = n.filter(&keep_even);
        let seed = g.constant(9u64);
        seed.merge(&evens).accumulate()
    };
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
    assert_eq!(r.value(&flat), r.value(&island), "nested == flat wiring");
}

wingfoil_next::graph! {
    // A statistics op (clock-aware, stateful) inside an island.
    fn ewma_island(g: &GraphBuilder) -> Stream<f64> {
        let x = g.ticker(Duration::from_nanos(100)).count().map(|i| *i as f64);
        let e = x.ewma_per_tick(0.5);
        e
    }
}

/// EWMA (alpha 0.5) seeded on the first sample over `1, 2, 3, 4` settles to
/// `3.125` — the same decay the flat statistics suite pins, now proving the op
/// works identically when mounted as a compiled island.
#[test]
fn island_statistics_ewma_all_paths() {
    let run_for = RunFor::Cycles(4);

    let (mut runner, e) = ewma_island::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(e);
    assert!(
        (interpreted - 3.125).abs() < 1e-10,
        "interpreted = {interpreted}"
    );

    let (compiled,) = ewma_island::compiled(HISTORICAL, run_for).unwrap();
    assert!(
        (compiled - interpreted).abs() < 1e-10,
        "interpreted == compiled"
    );

    let g = GraphBuilder::new();
    let island = ewma_island::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert!(
        (r.value(&island) - interpreted).abs() < 1e-10,
        "interpreted == nested"
    );
}
