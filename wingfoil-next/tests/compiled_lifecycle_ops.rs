//! **Lifecycle-hook ops in `compiled()` / `nested()`**: `print`, `timed` and
//! `finally` carry their observable effect in the *end-of-run* hooks
//! (`Op::stop` / `Op::teardown`), not in `cycle`. The interpreted `Runner` has
//! always run those hooks; this file pins that the two monomorphized engines —
//! standalone `compiled()` and the `nested()` island — now do too, and do so
//! with the *same* contract the interpreted engine honours: cleanup always
//! runs (even after a cycle aborts), in node order, first error wins.
//!
//! `finally` is the cleanly observable probe: its whole purpose is a
//! teardown closure. A shared thread-local counter lets each test assert the
//! closure fires **exactly once**, at run end, on all three engines — and a
//! shared order log asserts multiple lifecycle ops tear down in the same order
//! everywhere. `print` / `timed` are asserted through value pass-through parity
//! and clean completion (their diagnostics go to std{out,err}).

use std::cell::{Cell, RefCell};
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::anyhow::bail;
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

thread_local! {
    /// How many times a `finally` teardown closure has fired this run.
    static FINALLY_HITS: Cell<u32> = const { Cell::new(0) };
    /// The labels lifecycle hooks record as they tear down, in fire order.
    static TEARDOWN_ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn reset() {
    FINALLY_HITS.with(|c| c.set(0));
    TEARDOWN_ORDER.with(|o| o.borrow_mut().clear());
}
fn hits() -> u32 {
    FINALLY_HITS.with(Cell::get)
}
fn bump_hits() {
    FINALLY_HITS.with(|c| c.set(c.get() + 1));
}
fn record(label: &'static str) {
    TEARDOWN_ORDER.with(|o| o.borrow_mut().push(label));
}
fn order() -> Vec<&'static str> {
    TEARDOWN_ORDER.with(|o| o.borrow().clone())
}

// ---------------------------------------------------------------------------
// A single `finally` — the minimal teardown probe.
// ---------------------------------------------------------------------------

wingfoil_next::graph! {
    fn one_finally(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        // Side node: observes `count`, emits nothing, fires its closure once
        // at teardown. Not part of the tail, so it is a pure sink.
        let done = count.finally(|_v: &u64| {
            bump_hits();
            Ok(())
        });
        count
    }
}

/// `finally`'s teardown fires **exactly once, at run end** under every engine.
#[test]
fn finally_teardown_fires_once_on_all_three_engines() {
    // Interpreted — the reference.
    reset();
    let (mut runner, _count) = one_finally::interpreted();
    runner.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(1, hits(), "interpreted: finally must fire exactly once");

    // Compiled — standalone, owns its own run loop.
    reset();
    one_finally::compiled(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(1, hits(), "compiled: finally must fire exactly once");

    // Nested — the graph mounted as one island in an interpreted outer graph;
    // its inner teardown runs when the outer engine tears the island down.
    reset();
    let g = GraphBuilder::new();
    let _island = one_finally::nested(&g);
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(1, hits(), "nested: finally must fire exactly once");
}

// ---------------------------------------------------------------------------
// Multiple lifecycle ops — teardown order.
// ---------------------------------------------------------------------------

wingfoil_next::graph! {
    fn ordered_finallys(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        let a = count.finally(|_v: &u64| {
            record("a");
            Ok(())
        });
        let b = count.finally(|_v: &u64| {
            record("b");
            Ok(())
        });
        count
    }
}

/// Multiple lifecycle ops tear down in the **same order** on all three engines
/// — node (wiring) order, matching the interpreted `Runner`.
#[test]
fn teardown_order_matches_across_engines() {
    reset();
    let (mut runner, _count) = ordered_finallys::interpreted();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    let interpreted_order = order();
    assert_eq!(vec!["a", "b"], interpreted_order);

    reset();
    ordered_finallys::compiled(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(interpreted_order, order(), "compiled teardown order");

    reset();
    let g = GraphBuilder::new();
    let _island = ordered_finallys::nested(&g);
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(interpreted_order, order(), "nested teardown order");
}

// ---------------------------------------------------------------------------
// `print` + `timed` — pass-through parity and clean completion.
// ---------------------------------------------------------------------------

wingfoil_next::graph! {
    fn print_then_timed(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        // `print` buffers and flushes at teardown; `timed` records at start
        // and prints a summary at stop. Both pass their value through
        // unchanged, so the output stream is identical to `count`.
        let printed = count.print();
        let out = printed.timed();
        out
    }
}

/// `print` (teardown flush) and `timed` (start + stop hooks) pass values
/// through unchanged; every engine completes cleanly and yields the same
/// output as the interpreted reference.
#[test]
fn print_and_timed_pass_through_on_all_three_engines() {
    let (mut runner, out) = print_then_timed::interpreted();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let expected = runner.value(out);
    assert_eq!(6, expected);

    let (compiled,) = print_then_timed::compiled(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(expected, compiled, "compiled print/timed pass-through");

    let g = GraphBuilder::new();
    let island = print_then_timed::nested(&g);
    let handle = island.handle();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        expected,
        runner.value(handle),
        "nested print/timed pass-through"
    );
}

// ---------------------------------------------------------------------------
// Error-safe cleanup: a cycle abort still runs teardown, first error wins.
// ---------------------------------------------------------------------------

wingfoil_next::graph! {
    fn abort_then_finally(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        // Aborts the run the moment `count` reaches 3.
        let guarded = count.try_map(|c: &u64| {
            if *c >= 3 {
                bail!("boom at {c}");
            }
            Ok(*c)
        });
        let done = count.finally(|_v: &u64| {
            bump_hits();
            Ok(())
        });
        guarded
    }
}

/// A cycle error aborts the run but cleanup still runs: `compiled()` surfaces
/// the cycle error *and* fires `finally`'s teardown exactly once — the
/// interpreted "cleanup always runs, first error wins" contract, now honoured
/// by the compiled engine.
#[test]
fn compiled_cleanup_runs_after_cycle_abort() {
    reset();
    let (mut runner, _out) = abort_then_finally::interpreted();
    let interp_err = runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap_err();
    assert!(interp_err.to_string().contains("boom") || format!("{interp_err:?}").contains("boom"));
    assert_eq!(
        1,
        hits(),
        "interpreted: teardown runs despite the cycle abort"
    );

    reset();
    let compiled_err = abort_then_finally::compiled(HISTORICAL, RunFor::Cycles(6)).unwrap_err();
    assert!(
        format!("{compiled_err:?}").contains("boom"),
        "compiled surfaces the cycle error"
    );
    assert_eq!(1, hits(), "compiled: teardown runs despite the cycle abort");
}
