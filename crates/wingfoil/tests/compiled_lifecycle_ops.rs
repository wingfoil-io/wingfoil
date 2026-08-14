//! **Lifecycle-hook ops in `compiled()` / `nested()`**: `print`, `timed` and
//! `finally` carry their observable effect in the *end-of-run* hooks
//! (`Op::stop` / `Op::teardown`), not in `cycle`. The interpreted `Runner` has
//! always run those hooks; this file pins that the two monomorphized engines —
//! standalone `compiled()` and the `nested()` island — now do too, and do so
//! with the *same* contract the interpreted engine honours: cleanup always
//! runs (even after a cycle aborts), in node order, first error wins.
//!
//! `finally` is the cleanly observable probe for the **teardown** half: its
//! whole purpose is a teardown closure. A shared thread-local counter lets each
//! test assert the closure fires **exactly once**, at run end, on all three
//! engines — and a shared order log asserts multiple lifecycle ops tear down in
//! the same order everywhere. `print` / `timed` are asserted through value
//! pass-through parity and clean completion (their diagnostics go to
//! std{out,err}).
//!
//! The **stop** half needs its own probe, and no catalog op provides one:
//! `timed` is the only op with a real `Op::stop`, and its hook prints rather
//! than yielding anything a test can read. So [`Bookend`] below is a user op
//! (hand-written forwarders, the `tests/custom_op.rs` pattern) whose entire
//! observable behaviour *is* its `stop` hook, wired next to a `finally` so one
//! log pins both hooks *and* the ordering between them — every node's `stop`,
//! then every node's `teardown`, which is the interpreted `Runner`'s contract.
//! Until it existed, deleting the compiled/island `stop` emission left the
//! whole suite green.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use wingfoil::anyhow::{Result, bail};
use wingfoil::op;
use wingfoil::op::Op;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

thread_local! {
    /// How many times a `finally` teardown closure has fired this run.
    static FINALLY_HITS: Cell<u32> = const { Cell::new(0) };
    /// The labels lifecycle hooks record as they fire, in fire order.
    static LIFECYCLE_ORDER: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn reset() {
    FINALLY_HITS.with(|c| c.set(0));
    LIFECYCLE_ORDER.with(|o| o.borrow_mut().clear());
}
fn hits() -> u32 {
    FINALLY_HITS.with(Cell::get)
}
fn bump_hits() {
    FINALLY_HITS.with(|c| c.set(c.get() + 1));
}
fn record(label: impl Into<String>) {
    LIFECYCLE_ORDER.with(|o| o.borrow_mut().push(label.into()));
}
fn order() -> Vec<String> {
    LIFECYCLE_ORDER.with(|o| o.borrow().clone())
}

// ---------------------------------------------------------------------------
// A single `finally` — the minimal teardown probe.
// ---------------------------------------------------------------------------

wingfoil::nitro! {
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

wingfoil::nitro! {
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

wingfoil::nitro! {
    fn print_then_timed(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        // `print` prints each value per tick (deviation D8 — no teardown
        // buffer); `timed` records at start and prints a summary at stop. Both
        // pass their value through unchanged, so the output stream is identical
        // to `count`.
        let printed = count.print();
        let out = printed.timed();
        out
    }
}

/// `print` (per-tick print) and `timed` (start + stop hooks) pass values
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

wingfoil::nitro! {
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

// ---------------------------------------------------------------------------
// `Op::stop` — the other end-of-run hook, and the one no catalog op exposes
// observably. An ordinary user op: `#[op]` expands out-of-crate (#782), so it
// writes the forwarders *and* the interpreted wiring — including the `stop`
// hook — from the `Op` impl, in an integration test crate.
// ---------------------------------------------------------------------------

/// Pass-through op whose observable behaviour lives entirely in
/// [`Op::stop`]: at end of run it records `<label>:stop:<last value cycled>`.
///
/// Recording the *state* — not just the fact of the call — is deliberate: it
/// pins that the hook is handed the node's real, accumulated state, as the
/// interpreted engine hands its cycle and its stop the same cfg+state cell.
///
/// `Cfg` is the label as a `&'static str` so the `nitro!` call-site argument
/// type **is** the `Cfg` (the compiled emission uses call-site types verbatim);
/// an owned `String` config would make the op fluent-only.
pub struct Bookend;

#[op(build = bookend)]
impl Op for Bookend {
    type Cfg = &'static str;
    type State = u64;
    type In<'a> = (&'a u64,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut &'static str,
        state: &mut u64,
        input: (&u64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        *state = *input.0;
        Ok(Tick::Value(*state))
    }

    fn stop(cfg: &mut &'static str, state: &mut u64, _ctx: &mut Ctx<'_>) -> Result<()> {
        record(format!("{cfg}:stop:{state}"));
        Ok(())
    }
}

trait BookendOps {
    fn bookend(&self, label: &'static str) -> Stream<u64>;
}

impl BookendOps for Stream<u64> {
    fn bookend(&self, label: &'static str) -> Stream<u64> {
        // The generated `Builder::bookend` attaches the `stop` hook itself —
        // `#[op]` sees the impl override it. It is the interpreted twin of the
        // `_stop` forwarder the monomorphized tiers call, from one source.
        self.wire(move |b, h| b.bookend(h, label))
    }
}

wingfoil::nitro! {
    fn stop_then_teardown(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        // `bookend`'s effect is in `Op::stop`, `finally`'s in `Op::teardown`,
        // so one log covers both hooks and the boundary between them.
        let watched = count.bookend("bookend");
        let done = watched.finally(|v: &u64| {
            record(format!("finally:teardown:{v}"));
            Ok(())
        });
        watched
    }
}

/// `Op::stop` fires **exactly once, with the node's real state, before any
/// node's `teardown`** — on all three engines.
///
/// This is the regression guard for the `stop` half of the `nitro!` end-of-run
/// emission: with the compiled or island `stop` calls removed, the two
/// monomorphized engines log only `finally:teardown:5` and this test fails
/// where every other test still passes.
#[test]
fn stop_then_teardown_ordering_matches_across_engines() {
    let run_for = RunFor::Cycles(5);
    let expected = vec!["bookend:stop:5", "finally:teardown:5"];

    // Interpreted — the oracle: every node's `stop`, then every node's
    // `teardown`, each in node order.
    reset();
    let (mut runner, out) = stop_then_teardown::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    assert_eq!(5, runner.value(out));
    let interpreted_order = order();
    assert_eq!(expected, interpreted_order);

    reset();
    let (compiled,) = stop_then_teardown::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(5, compiled, "compiled pass-through");
    assert_eq!(
        interpreted_order,
        order(),
        "compiled: stop must run (with real state) before teardown"
    );

    reset();
    let g = GraphBuilder::new();
    let island = stop_then_teardown::nested(&g);
    let handle = island.handle();
    let mut runner = g.build();
    runner.run(HISTORICAL, run_for).unwrap();
    assert_eq!(5, runner.value(handle), "nested pass-through");
    assert_eq!(
        interpreted_order,
        order(),
        "nested: the island's inner stop then teardown, driven by the outer run"
    );
}
