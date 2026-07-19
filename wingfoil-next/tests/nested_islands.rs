//! Compiled islands inside interpreted graphs: the `graph!` macro's
//! `nested` expansion mounts an entire compiled sub-graph as a **single
//! node** of an interpreted graph. The outer engine pays one dyn call per
//! activation for the whole island; inner dispatch is the same
//! monomorphized straight-line code `compiled()` emits.
//!
//! Each test cross-checks the island against the identical wiring done
//! flat (all-interpreted) in the same graph, so the two execution paths
//! run under the same kernel, on the same cycles, and must agree exactly.
//!
//! (Timing note for the expectations below: a ticker's first fire is at the
//! run's start time, so a 100ns ticker started at t=0 fires at 0, 100, ...)

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

wingfoil_next::graph! {
    fn scaled_sum(g: &GraphBuilder, src: &Stream<u64>) -> Stream<u64> {
        let doubled = src.map(|i| i * 2);
        let summed = doubled.fold(0u64, |acc, v| *acc += v);
        summed
    }
}

/// An input-taking island: the outer graph's count flows through the
/// island's map + fold and back out, in lockstep with the identical flat
/// wiring fed by the same source.
#[test]
fn island_with_input_matches_flat_wiring() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();

    let island = scaled_sum::nested(&g, &count).accumulate();
    let flat = count
        .map(|i| i * 2)
        .fold(0u64, |acc, v| *acc += v)
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    let flat_values = r.value(&flat);
    assert_eq!(vec![2, 6, 12, 20], flat_values);
    assert_eq!(flat_values, r.value(&island));
}

wingfoil_next::graph! {
    fn delayed(g: &GraphBuilder, src: &Stream<u64>) -> Stream<u64> {
        let out = src.delay(Duration::from_nanos(100));
        out
    }
}

/// An island whose inner op self-schedules (delay): its callbacks are
/// demultiplexed through the island's private queue and forwarded to the
/// outer kernel — matching a flat delay running beside it under the same
/// kernel.
#[test]
fn island_delay_schedules_through_outer_kernel() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();

    let island = delayed::nested(&g, &count).accumulate();
    let flat = count.delay(Duration::from_nanos(100)).accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(120)))
        .unwrap();
    let flat_values = r.value(&flat);
    assert!(!flat_values.is_empty());
    assert_eq!(flat_values, r.value(&island));
}

wingfoil_next::graph! {
    fn pulse(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(Duration::from_nanos(50)).count();
        let scaled = count.map(|i| i * 10);
        scaled
    }
}

/// A source island: no inputs, its internal ticker drives the outer graph
/// entirely through forwarded schedules.
#[test]
fn source_island_drives_the_outer_graph() {
    let g = GraphBuilder::new();
    let acc = pulse::nested(&g).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![10, 20, 30], r.value(&acc));
}

/// A source island still exposes the standalone expansions — all three
/// engines (interpreted, compiled, nested-in-interpreted) agree.
#[test]
fn source_island_agrees_with_standalone_expansions() {
    let run_for = RunFor::Cycles(5);

    let (mut runner, scaled) = pulse::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(scaled);

    let (compiled,) = pulse::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);

    let g = GraphBuilder::new();
    let island = pulse::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, r.value(&island));
}

wingfoil_next::graph! {
    fn gated(g: &GraphBuilder, data: &Stream<u64>, trigger: &Stream<()>) -> Stream<u64> {
        let sampled = data.sample(trigger);
        sampled
    }
}

/// A passively read input must not activate the island: `data` changes
/// every 10ns but the island only emits on the 100ns trigger (at 0, 100,
/// 200, 300 — sampling counts 1, 11, 21, 31), exactly like the flat sample
/// beside it.
#[test]
fn passive_input_does_not_activate_the_island() {
    let g = GraphBuilder::new();
    let data = g.ticker(Duration::from_nanos(10)).count();
    let trigger = g.ticker(Duration::from_nanos(100));

    let island = gated::nested(&g, &data, &trigger).accumulate();
    let flat = data.sample(&trigger).accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(305)))
        .unwrap();
    let flat_values = r.value(&flat);
    assert_eq!(vec![1, 11, 21, 31], flat_values);
    assert_eq!(flat_values, r.value(&island));
}
