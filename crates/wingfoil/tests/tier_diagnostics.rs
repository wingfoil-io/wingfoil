//! **Tier ergonomics**: the two things that made a `nitro!` graph harder to
//! develop against than to run.
//!
//! 1. **Per-node error context in the monomorphized tiers.** The interpreted
//!    engine has always wrapped every lifecycle hook with
//!    `node {i} ({label}) {hook}`; `compiled()` and `nested()` propagated bare,
//!    so the identical graph reported which node failed under the engine you
//!    develop on and stayed silent under the one you deploy. Both now name the
//!    node — and name it *better*, because the macro knows the binding the user
//!    wrote (`doomed: try_map`) where the interpreted engine only has the op's
//!    `type_name` (`TryMap`).
//!
//! 2. **A tier-agnostic entry point.** `interpreted()` and `compiled()` are
//!    shaped differently on purpose (runner + handles vs. run bounds + values),
//!    which is exactly what stopped a caller swapping engines behind a flag.
//!    `run(tier, run_mode, run_for)` reconciles them, and the tests below pin
//!    that it agrees with both underlying entry points — values *and* errors.
//!
//! The probes are catalog ops, not bespoke test ops: `try_map` is the fallible
//! `cycle`, `finally` the fallible `teardown`. Both reach all three tiers
//! through the ordinary `#[op(build = ..)]` forwarders.

use std::time::Duration;

use wingfoil::anyhow::bail;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode, Tier};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

/// The full `anyhow` chain — `{:?}` on an `Error` prints the outermost context
/// plus every cause, which is where the node label and the op's own message
/// both live.
fn chain(e: &wingfoil::anyhow::Error) -> String {
    format!("{e:?}")
}

// ---------------------------------------------------------------------------
// A cycle abort: `try_map` bails on the third value.
// ---------------------------------------------------------------------------

// Node order is wiring order: 0 = the anonymous ticker, 1 = `counter`,
// 2 = `doomed`. The label under test is therefore `node 2 (doomed: try_map)`.
wingfoil::nitro! {
    fn boom_in_cycle(g: &GraphBuilder) -> Stream<u64> {
        let counter = g.ticker(PERIOD).count();
        let doomed = counter.try_map(|c: &u64| {
            if *c >= 3 {
                bail!("kaboom at {c}");
            }
            Ok(*c)
        });
        doomed
    }
}

#[test]
fn compiled_cycle_error_names_the_node() {
    let err = boom_in_cycle::compiled(HISTORICAL, RunFor::Cycles(6)).unwrap_err();
    let text = chain(&err);
    assert!(
        text.contains("node 2 (doomed: try_map) cycle"),
        "compiled cycle error must name the node and hook, got:\n{text}"
    );
    assert!(
        text.contains("kaboom at 3"),
        "the op's own message must survive the added context, got:\n{text}"
    );
}

#[test]
fn island_cycle_error_names_the_node_and_says_it_is_an_island() {
    let g = GraphBuilder::new();
    let _island = boom_in_cycle::nested(&g);
    let mut runner = g.build();
    let err = runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap_err();
    let text = chain(&err);
    assert!(
        text.contains("island node 2 (doomed: try_map) cycle"),
        "island cycle error must name the inner node, got:\n{text}"
    );
    assert!(
        text.contains("kaboom at 3"),
        "the op's own message must survive the added context, got:\n{text}"
    );
}

/// The interpreted engine keeps its own wording (op `type_name`, not the
/// binding name) — the macro cannot match it without naming the op type, which
/// is precisely what the open-op-set design removes. What must hold is that
/// *every* tier names the failing node and hook somehow, so nobody debugging a
/// failure has to guess based on which engine they happened to run.
#[test]
fn every_tier_names_the_failing_node() {
    let interpreted = {
        let (mut runner, _out) = boom_in_cycle::interpreted();
        chain(&runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap_err())
    };
    assert!(
        interpreted.contains("node 2") && interpreted.contains("cycle"),
        "interpreted must name the node and hook, got:\n{interpreted}"
    );

    for tier in [Tier::Interpreted, Tier::Compiled] {
        let text = chain(&boom_in_cycle::run(tier, HISTORICAL, RunFor::Cycles(6)).unwrap_err());
        assert!(
            text.contains("node 2") && text.contains("cycle") && text.contains("kaboom at 3"),
            "{tier} tier must name the node, the hook and the cause, got:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// A teardown abort: `finally` bails at end of run.
// ---------------------------------------------------------------------------

// 0 = anonymous ticker, 1 = `counter`, 2 = `sink`.
wingfoil::nitro! {
    fn boom_in_teardown(g: &GraphBuilder) -> Stream<()> {
        let counter = g.ticker(PERIOD).count();
        let sink = counter.finally(|_v: &u64| {
            bail!("teardown kaboom");
        });
        sink
    }
}

#[test]
fn compiled_teardown_error_names_the_node_and_hook() {
    let err = boom_in_teardown::compiled(HISTORICAL, RunFor::Cycles(3)).unwrap_err();
    let text = chain(&err);
    assert!(
        text.contains("node 2 (sink: finally) teardown"),
        "compiled teardown error must name the node and the *teardown* hook, got:\n{text}"
    );
    assert!(
        text.contains("teardown kaboom"),
        "the op's own message must survive the added context, got:\n{text}"
    );
}

#[test]
fn island_teardown_error_names_the_node_and_hook() {
    let g = GraphBuilder::new();
    let _island = boom_in_teardown::nested(&g);
    let mut runner = g.build();
    let text = chain(&runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap_err());
    assert!(
        text.contains("island node 2 (sink: finally) teardown"),
        "island teardown error must name the inner node and hook, got:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// `run(tier, ..)`: one call shape, either engine, identical outputs.
// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn odds_evens(g: &GraphBuilder) -> Stream<Vec<String>> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        let is_odd = is_even.map(|b| !b);
        let odd_str = count.filter(&is_odd).map(|i| format!("{i} is odd"));
        let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
        let acc = odd_str.merge(&even_str).accumulate();
        acc
    }
}

/// The whole point of `run`: swapping tiers is an argument, and the outputs it
/// returns are the ones the tier-specific entry points return — so a caller
/// that develops on `Interpreted` and deploys on `Compiled` is running the
/// same assertions against the same values.
#[test]
fn run_agrees_with_both_tier_specific_entry_points() {
    let run_for = RunFor::Cycles(10);

    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let via_interpreted = runner.value(acc);
    let (via_compiled,) = odds_evens::compiled(HISTORICAL, run_for).unwrap();

    let (run_interpreted,) = odds_evens::run(Tier::Interpreted, HISTORICAL, run_for).unwrap();
    let (run_compiled,) = odds_evens::run(Tier::Compiled, HISTORICAL, run_for).unwrap();

    assert_eq!(via_interpreted, via_compiled, "the two engines must agree");
    assert_eq!(run_interpreted, via_interpreted);
    assert_eq!(run_compiled, via_compiled);
    assert_eq!(10, run_compiled.len());
}

/// `Tier::default()` reads the environment, so it is deliberately *not*
/// asserted to a particular variant here — that would make this test depend on
/// how the suite happens to be invoked. What it must do is resolve to a tier
/// that runs the graph to the same answer as an explicitly named one.
#[test]
fn default_tier_runs_the_graph() {
    let run_for = RunFor::Cycles(10);
    let (via_default,) = odds_evens::run(Tier::default(), HISTORICAL, run_for).unwrap();
    let (via_compiled,) = odds_evens::run(Tier::Compiled, HISTORICAL, run_for).unwrap();
    assert_eq!(via_default, via_compiled);
}
