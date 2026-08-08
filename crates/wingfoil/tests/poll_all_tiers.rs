//! **A busy-poll source across all three tiers.** `poll` is the first
//! `Activation::ALWAYS` op to reach `nitro!`, and `always` is the one
//! activation the compiled tiers could not previously honour: `node_dispatch`
//! already cycles such a node unconditionally, but a realtime `begin_cycle`
//! *parks* until the next scheduled callback, so a graph whose only source
//! polls would sleep rather than spin. The interpreted `Runner` sets
//! `Kernel::set_spin` from its `has_always` flag; the compiled expansion now
//! derives the same fact from the OR of its ops' `ACTIVATION` consts, and a
//! `nested()` island declares it outward so the *outer* engine spins.
//!
//! The parity assertion is what pins all three: an island that failed to
//! declare `always` outward is never cycled at all (the composite is one node
//! with no upstream edge to activate it), which shows up here as a zero.

use std::cell::Cell;

use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(0) };
}

fn reset() {
    NEXT.with(|c| c.set(0));
}

wingfoil::nitro! {
    fn polled(g: &GraphBuilder) -> Stream<u64> {
        // Ticks every cycle with a monotonically increasing value.
        let src = g.poll(|| {
            NEXT.with(|c| {
                let v = c.get() + 1;
                c.set(v);
                Some(v)
            })
        });
        let doubled = src.map(|v: &u64| v * 2);
        doubled
    }
}

#[test]
fn poll_source_runs_in_all_three_tiers() {
    // Interpreted — the reference.
    reset();
    let (mut runner, out) = polled::interpreted();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(5))
        .expect("interpreted run");
    let interpreted = runner.value(out);
    assert_eq!(10, interpreted, "interpreted: 5 polls, doubled");

    // Compiled — standalone, owns its own kernel and run loop.
    reset();
    let (compiled,) = polled::compiled(RunMode::RealTime, RunFor::Cycles(5)).expect("compiled run");
    assert_eq!(interpreted, compiled, "compiled must match interpreted");

    // Nested — the same graph mounted as one island in an interpreted graph.
    reset();
    let g = GraphBuilder::new();
    let island = polled::nested(&g);
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(5))
        .expect("nested run");
    let nested = runner.value(island);
    assert_eq!(interpreted, nested, "nested must match interpreted");
}

/// A poll source is wall-clock, so a historical run must be rejected — the same
/// contract the interpreted `Runner` enforces.
#[test]
fn poll_in_compiled_rejects_historical() {
    reset();
    let err = polled::compiled(
        RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
        RunFor::Cycles(5),
    )
    .expect_err("historical must be rejected");
    assert!(
        err.to_string().contains("require RunMode::RealTime"),
        "unexpected error: {err}"
    );
}
