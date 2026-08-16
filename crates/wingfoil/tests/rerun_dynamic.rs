//! Re-run contract for [`Runner::run_dynamic`](wingfoil::interp::Runner::run_dynamic).
//!
//! `run_dynamic` is a second public entry point into the same runner, and it
//! used to keep its own copy of `run`'s preamble — minus the re-run guard. So
//! it neither set nor checked `has_run`, and every ordering silently did the
//! wrong thing:
//!
//! * `run` → `run_dynamic` on a single-run *historical channel* graph returned
//!   `Ok(())` having done nothing at all: the `finished` flag set by the
//!   channel's `EndOfStream` is cleared only by `reset`, which never ran, so the
//!   cycle loop exited on its first test.
//! * every re-runnable ordering (`run` → `run_dynamic`, `run_dynamic` →
//!   `run_dynamic`, `run_dynamic` → `run`) continued stale fold/accumulator
//!   state instead of restoring it.
//!
//! Both entry points now share one guard, so the contract is `run`'s in either
//! direction: re-runnable graphs reset, single-run graphs error.
//!
//! The last test covers the *removed* half of that contract, which the guard
//! alone does not settle: `reset` clears neither a node's tombstone nor the
//! edges its removal drained, so a node deleted through [`Extension::remove`]
//! must stay out of the second run's lifecycle entirely — `start` included.
#![cfg(feature = "dynamic-graph")]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::anyhow::Result;
use wingfoil::interp::Extension;
use wingfoil::op;
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const TEN: Duration = Duration::from_nanos(10);

/// No-op mutation hook — `run_dynamic` with nothing to mutate, i.e. exactly
/// `run` plus the boundary callback.
fn nothing(_ext: &mut Extension<'_>, _cycle: u64) -> anyhow::Result<()> {
    Ok(())
}

/// A stateful re-runnable graph: a `count` fold and its tick times. The
/// accumulated `(time, value)` pairs pin both halves of the contract — values
/// *and* tick times must reproduce.
fn wire(g: &GraphBuilder) -> (Stream<Vec<(NanoTime, u64)>>, Stream<u64>) {
    let count = g.ticker(TEN).count();
    let sum = count.fold(0u64, |acc, x| *acc += *x);
    (count.with_time().accumulate(), sum)
}

/// The pinned sequence of a 4-cycle run of [`wire`] — ticker at 10ns, whose
/// first callback is scheduled at the run's start time and whose first cycle
/// therefore fires at t=0 (`Kernel::begin_cycle`: "first cycle fires at the
/// callback's own time").
fn expected() -> Vec<(NanoTime, u64)> {
    vec![
        (NanoTime::new(0), 1),
        (NanoTime::new(10), 2),
        (NanoTime::new(20), 3),
        (NanoTime::new(30), 4),
    ]
}

/// `run` then `run_dynamic`: the second run resets, so it reproduces the first
/// exactly rather than continuing the fold.
#[test]
fn run_then_run_dynamic_resets() {
    let g = GraphBuilder::new();
    let (acc, sum) = wire(&g);
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(expected(), r.value(&acc));
    assert_eq!(1 + 2 + 3 + 4, r.value(&sum));

    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    assert_eq!(
        expected(),
        r.value(&acc),
        "run_dynamic after run must reset, not continue"
    );
    // Stale state would have left the fold at 10 + 10 = 20.
    assert_eq!(10, r.value(&sum));
}

/// `run_dynamic` then `run_dynamic`: same contract between two dynamic runs.
#[test]
fn run_dynamic_twice_resets() {
    let g = GraphBuilder::new();
    let (acc, sum) = wire(&g);
    let mut r = g.build();

    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    assert_eq!(expected(), r.value(&acc));
    assert_eq!(10, r.value(&sum));

    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    assert_eq!(expected(), r.value(&acc), "a second run_dynamic must reset");
    assert_eq!(10, r.value(&sum));
}

/// `run_dynamic` then `run`: `run_dynamic` must *set* `has_run`, or the plain
/// `run` that follows skips its reset and continues stale state.
#[test]
fn run_dynamic_then_run_resets() {
    let g = GraphBuilder::new();
    let (acc, sum) = wire(&g);
    let mut r = g.build();

    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    assert_eq!(expected(), r.value(&acc));
    assert_eq!(10, r.value(&sum));

    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(
        expected(),
        r.value(&acc),
        "run after run_dynamic must reset, not continue"
    );
    assert_eq!(10, r.value(&sum));
}

/// A dynamic run and a plain run of the same re-runnable graph produce
/// identical values *and* tick times — the boundary hook is the only
/// difference between the two entry points.
#[test]
fn run_dynamic_matches_run() {
    let g = GraphBuilder::new();
    let (acc, _sum) = wire(&g);
    let mut r = g.build();
    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    let dynamic = r.value(&acc);

    let g2 = GraphBuilder::new();
    let (acc2, _sum2) = wire(&g2);
    let mut r2 = g2.build();
    r2.run(HISTORICAL, RunFor::Cycles(4)).unwrap();

    assert_eq!(r2.value(&acc2), dynamic);
    assert_eq!(expected(), dynamic);
}

/// The graph a re-run re-runs is the one the first run *left behind* — a node
/// appended through the hook is still wired, and fires from cycle 1 of the
/// second run. Only the *statically built* region is restored, though: a
/// dynamically-appended node carries a no-op reset by construction
/// (`Runner::rt_append_node`), so it re-runs carrying its own state. Appended at
/// the end of cycle 2 of run 1 it fires on cycles 3 and 4 (2 activations), then
/// on all four cycles of run 2 — 6, not 4. This pins the documented caveat on
/// `run_dynamic`, so a future change to dynamic-node reset is a deliberate one.
#[test]
fn appended_node_survives_run_but_keeps_its_state() {
    let g = GraphBuilder::new();
    let src = g.ticker(TEN).count().handle();
    let mut r = g.build();

    let mut appended = None;
    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), |ext, cycle| {
        if cycle == 2 && appended.is_none() {
            appended = Some(ext.fold(src, 0u64, |acc, _| *acc += 1));
        }
        Ok(())
    })
    .unwrap();
    let appended = appended.expect("node appended at cycle 2");
    assert_eq!(2, r.value(appended), "fired on cycles 3 and 4 only");

    // Second run: already wired, so it fires all four cycles — but its own
    // state is not restored, so it continues from 2 rather than restarting.
    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    assert_eq!(
        2 + 4,
        r.value(appended),
        "an appended node keeps its state across a re-run (no-op reset)"
    );
    // The statically-built source *did* reset — 4, not 8.
    assert_eq!(4, r.value(src));
}

/// The `finished` case. A historical channel graph is **single-run**: the
/// channel's `EndOfStream` sets `finished`, which only `reset` clears, and
/// `reset` cannot restore the consumed producer channel anyway. `run_dynamic`
/// after `run` must therefore error — it used to return `Ok(())` having run
/// zero cycles, leaving the first run's values in place and looking like a
/// success.
#[test]
fn historical_channel_graph_refuses_run_dynamic_after_run() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.with_time().accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(10, NanoTime::new(100));
        sender.send_at(20, NanoTime::new(200));
        sender.close();
    });
    r.run(HISTORICAL, RunFor::Forever).unwrap();
    producer.join().expect("producer thread");

    let flat: Vec<(NanoTime, Vec<u64>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        vec![
            (NanoTime::new(100), vec![10]),
            (NanoTime::new(200), vec![20]),
        ],
        flat
    );

    let err = r
        .run_dynamic(HISTORICAL, RunFor::Forever, nothing)
        .expect_err("a single-run channel graph must refuse run_dynamic after run");
    assert!(
        err.to_string().contains("single-run"),
        "unexpected error: {err}"
    );
}

/// The mirror image: `run_dynamic` first, then `run` — the single-run guard
/// fires whichever entry point consumed the graph.
#[test]
fn historical_channel_graph_refuses_run_after_run_dynamic() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.with_time().accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(7, NanoTime::new(50));
        sender.close();
    });
    r.run_dynamic(HISTORICAL, RunFor::Forever, nothing).unwrap();
    producer.join().expect("producer thread");

    let flat: Vec<(NanoTime, Vec<u64>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(vec![(NanoTime::new(50), vec![7])], flat);

    let err = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("a single-run channel graph must refuse a second run");
    assert!(
        err.to_string().contains("single-run"),
        "unexpected error: {err}"
    );

    let err = r
        .run_dynamic(HISTORICAL, RunFor::Forever, nothing)
        .expect_err("...and a second run_dynamic likewise");
    assert!(
        err.to_string().contains("single-run"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// The removed half of the re-run contract.
// ---------------------------------------------------------------------------

/// Lifecycle call counters, shared between the graph and the test. `Rc<Cell>`
/// rather than atomics because an interpreted graph runs on the calling thread.
#[derive(Clone, Default)]
struct Calls {
    start: Rc<Cell<u64>>,
    stop: Rc<Cell<u64>>,
    teardown: Rc<Cell<u64>>,
}

/// A pass-through op that exists only to make its own lifecycle observable: it
/// counts every `start`/`stop`/`teardown` the engine calls on it. Stands in for
/// any op that *acquires* something on start — the leak the tombstone skip
/// prevents is one unmatched `start` per re-run.
struct Tracked;

#[op(build = tracked)]
impl Op for Tracked {
    type Cfg = Calls;
    type State = ();
    type In<'a> = (&'a u64,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Calls,
        _state: &mut (),
        input: (&u64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        Ok(Tick::Value(*input.0))
    }

    fn start(cfg: &mut Calls, _state: &mut (), _ctx: &mut Ctx<'_>) -> Result<()> {
        cfg.start.set(cfg.start.get() + 1);
        Ok(())
    }

    fn stop(cfg: &mut Calls, _state: &mut (), _ctx: &mut Ctx<'_>) -> Result<()> {
        cfg.stop.set(cfg.stop.get() + 1);
        Ok(())
    }

    fn teardown(cfg: &mut Calls, _state: &mut (), _ctx: &mut Ctx<'_>) -> Result<()> {
        cfg.teardown.set(cfg.teardown.get() + 1);
        Ok(())
    }
}

/// `run_dynamic` → `remove` → `run_dynamic`: a tombstoned node takes no part in
/// the second run's lifecycle, so its `start` and `stop` counts stay balanced.
///
/// The removal runs the node's `stop`/`teardown` once, at the cycle-2 boundary
/// of run 1 (`dynamic_graph.rs`'s `remove_runs_teardown_once_at_removal` pins
/// that end). Run 2 then resets the graph — but `reset` clears neither the
/// tombstone nor the edges the removal drained, so the node is both unwired and
/// unstartable. `start_nodes` skips it exactly as `stop_and_teardown` does; it
/// used to call `start` unconditionally, giving a second `start` against one
/// `stop` and leaking whatever that start acquired for the rest of the process.
#[test]
fn removed_node_is_not_restarted_on_a_re_run() {
    let calls = Calls::default();
    let g = GraphBuilder::new();
    let counted = g.ticker(TEN).count();
    let cfg = calls.clone();
    let tracked = counted.wire(move |b, h| b.tracked(h, cfg));
    let acc = tracked.with_time().accumulate();
    let victim = tracked.handle();
    let mut r = g.build();

    // Run 1: the node fires cycles 1 and 2, then is removed at the cycle-2
    // boundary and never fires again.
    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), |ext, cycle| {
        if cycle == 2 {
            ext.remove(victim)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        vec![(NanoTime::new(0), 1u64), (NanoTime::new(10), 2)],
        r.value(&acc),
        "removed at the cycle-2 boundary: values and tick times stop there"
    );
    assert_eq!(1, calls.start.get(), "started once, by run 1");
    assert_eq!(1, calls.stop.get(), "stopped once, at removal");
    assert_eq!(1, calls.teardown.get(), "torn down once, at removal");

    // Run 2: the tombstone survives `reset`, so the node is skipped by every
    // lifecycle phase — no second `start`, and therefore no imbalance.
    r.run_dynamic(HISTORICAL, RunFor::Cycles(4), nothing)
        .unwrap();
    assert_eq!(
        1,
        calls.start.get(),
        "a tombstoned node must not be re-started by a second run"
    );
    assert_eq!(1, calls.stop.get(), "…and is still not re-stopped");
    assert_eq!(
        calls.start.get(),
        calls.stop.get(),
        "start/stop stay balanced across the re-run"
    );
    assert_eq!(1, calls.teardown.get());

    // Its downstream lost its only upstream at removal and `reset` cannot
    // rebuild that edge, so the whole removed region is silent in run 2 — the
    // accumulator resets to empty and never ticks again.
    assert_eq!(
        Vec::<(NanoTime, u64)>::new(),
        r.value(&acc),
        "the removed region is unwired for good; reset restores it to empty"
    );
}
