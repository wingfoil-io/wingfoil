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

// ---------------------------------------------------------------------------
// And the fourth path: **generated**. Once a busy-poll source is expressible in
// `nitro!`, the two-pass generator can reach it too — so an ingest graph whose
// *shape* comes from run-time config can be compiled.
//
// That needs one thing beyond the tiers above: the walker has to know the op's
// method name. `poll`'s builder is hand-written (it sets `has_always` and
// `re_runnable`, which a generated one cannot), so `#[op(no_builder)]`
// suppresses the generated builder and with it the `set_node_build` call that
// would ordinarily record `"poll"`. It is recorded by hand instead.
// ---------------------------------------------------------------------------

// The artifact `a_poll_source_generates_a_valid_artifact` must emit, byte for
// byte. It compiles, so the emitted text is valid wiring source by construction
// — the same device `tests/codegen_emission.rs` uses.
wingfoil::nitro! {
    fn polled_generated(g: &GraphBuilder) -> Stream<u64> {
        let n0_poll = g.poll({ let seed = 3u64; move || Some(seed) });
        let n1_map = n0_poll.map(|v: &u64| v * 2);
        n1_map
    }
}

/// A busy-poll ingest graph, wired procedurally and emitted.
#[test]
fn a_poll_source_generates_a_valid_artifact() {
    #[wingfoil::wiring]
    fn feed(g: &GraphBuilder, seed: u64) -> Stream<u64> {
        g.poll(move || Some(seed)).map(|v: &u64| v * 2)
    }

    let src = wingfoil::codegen::generate("polled_generated", "u64", |g| feed(g, 3))
        .expect("a poll source is emittable");

    let expected = concat!(
        "wingfoil::nitro! {\n",
        "    fn polled_generated(g: &GraphBuilder) -> Stream<u64> {\n",
        "        let n0_poll = g.poll({ let seed = 3u64; move || Some(seed) });\n",
        "        let n1_map = n0_poll.map(|v: &u64| v * 2);\n",
        "        n1_map\n",
        "    }\n",
        "}",
    );
    assert_eq!(expected, src);

    // `seed` existed only in the wiring, so the artifact re-materialises it as
    // its own binding rather than referring outward — which is what makes the
    // emitted block resolve wherever it is compiled.
    assert!(
        src.contains("{ let seed = 3u64; move || Some(seed) }"),
        "{src}"
    );
}

/// Parity for the generated artifact, on the tier that matters: the compiled
/// one. Realtime only, so this asserts values rather than tick times.
#[test]
fn the_generated_poll_graph_matches_its_wiring() {
    #[wingfoil::wiring]
    fn feed(g: &GraphBuilder, seed: u64) -> Stream<u64> {
        g.poll(move || Some(seed)).map(|v: &u64| v * 2)
    }

    let g = GraphBuilder::new();
    let out = feed(&g, 3);
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(5))
        .expect("wiring run");
    let wired = runner.value(out);

    let (generated,) =
        polled_generated::compiled(RunMode::RealTime, RunFor::Cycles(5)).expect("compiled run");
    assert_eq!(6, wired, "3 polled, doubled");
    assert_eq!(wired, generated, "the artifact must match its wiring");
}

/// An **unquoted** poll closure is still refused — recording the build name
/// must not turn a source whose closure the engine erased into a bare
/// `g.poll()` with no argument.
#[test]
fn an_unquoted_poll_closure_is_still_refused() {
    let g = GraphBuilder::new();
    let _ = g.poll(|| Some(1u64));
    let bad = wingfoil::codegen::ineligible(&g.describe());
    assert_eq!(1, bad.len(), "{bad:?}");
    assert!(bad[0].reason.contains("poll"), "{:?}", bad[0]);
    assert!(
        !bad[0].reason.contains("hand-written node"),
        "the advice must be about quoting, not variadics: {:?}",
        bad[0]
    );
}
