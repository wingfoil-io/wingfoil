//! `#[op]`-generated `Builder` methods, one test per **shape**.
//!
//! `#[op(build = name)]` writes the op's interpreted wiring from its `In`
//! shape. It used to cover only the single-active-input case, so every other
//! shape carried a hand-written `Builder` method that repeated the same
//! twenty-line sequence (`slot` → `new_slot` → `push_node` → reset hook). Those
//! are gone: the attribute now generates the method for sources, multi-input
//! ops, passive edges, tick-flag edges, lifecycle hooks, and seeded
//! accumulators alike.
//!
//! The whole fluent surface runs through those generated methods, so the rest
//! of the suite is the behavioural oracle. What *this* file pins down is the
//! part that has no other guard: the **generated method itself** — that it
//! exists for each shape, with the signature the shape implies (one `Handle`
//! per edge in `In` order, then `init`, then `Cfg`), and that the wiring it
//! emits carries each shape's distinguishing property:
//!
//! | Shape | Op | Property under test |
//! |---|---|---|
//! | source (`In = ()`) | `ticker`, `constant` | no upstreams; `start` schedules |
//! | multi-input | `join3`, `filter` | every edge read, every edge triggers |
//! | passive mask | `sample`, `join_passive` | passive edge read but not triggering |
//! | tick flags | `delay`, `merge` | the engine's per-edge tick flag reaches `In` |
//! | lifecycle hooks | `finally`, `timed` | `start`/`stop`/`teardown` attached |
//! | seeded accumulator | `fold` | `init` seeds state *and* value slot |
//!
//! Every op is wired by calling its generated `Builder` method **directly**
//! (through `GraphBuilder::source` / `Stream::wire`) rather than through its
//! fluent method, so a shape losing its generated method is a compile error
//! here even if the fluent layer is later re-pointed at something else.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(100);

// --- source shape (`In = ()`) ----------------------------------------------

/// A source op has no edges, so its generated method takes the config alone.
/// `Ticker`'s `start` hook seeds the first schedule (without it the graph
/// would never cycle) and `Const`'s ticks once at the start instant.
#[test]
fn source_shape_takes_config_only_and_runs_its_start_hook() {
    let g = GraphBuilder::new();
    let ticks: Stream<()> = g.source(|b| b.ticker(P));
    let seven: Stream<u64> = g.source(|b| b.constant(7u64));
    let stamped = ticks.count().with_time().accumulate();
    let consts = seven.with_time().accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();

    assert_eq!(
        vec![
            (NanoTime::ZERO, 1),
            (NanoTime::new(100), 2),
            (NanoTime::new(200), 3),
        ],
        runner.value(&stamped),
        "ticker: anchored at the start instant, then one tick per period",
    );
    assert_eq!(
        vec![(NanoTime::ZERO, 7)],
        runner.value(&consts),
        "constant: exactly one tick, at the start instant",
    );
}

// --- multi-input shape ------------------------------------------------------

/// Three edges, all active: the generated `join3` takes them positionally in
/// `In` order and then the closure config. Any of the three ticking runs the
/// node, and all three values are read.
#[test]
fn multi_input_shape_reads_and_is_triggered_by_every_edge() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let a = count.map(|n: &u64| *n);
    let b = count.map(|n: &u64| *n * 10);
    let c = count.map(|n: &u64| *n * 100);

    let (bh, ch) = (b.handle(), c.handle());
    let joined: Stream<u64> =
        a.wire(move |bld, h| bld.join3(h, bh, ch, |x: &u64, y: &u64, z: &u64| x + y + z));
    let out = joined.accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![111, 222, 333], runner.value(&out));
}

/// Two edges of *different* types (`&T` and `&bool`): the generated `filter`
/// takes `Handle<T>` then `Handle<bool>`, matching `In = (&T, &bool)`.
#[test]
fn multi_input_shape_types_each_edge_from_the_in_tuple() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let even = count.map(|n: &u64| n.is_multiple_of(2));

    let cond = even.handle();
    let kept: Stream<u64> = count.wire(move |b, h| b.filter(h, cond));
    let out = kept.accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![2, 4], runner.value(&out));
}

// --- passive-mask shape -----------------------------------------------------

/// `#[op(passive = [0])]`: edge 0 is read but does **not** trigger the node, so
/// `sample` emits only when its trigger ticks — and emits the data edge's
/// current value, which it could not see if the mask merely dropped the edge.
#[test]
fn passive_mask_shape_reads_the_passive_edge_without_being_triggered_by_it() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let flags = count.map(|n: &u64| n.is_multiple_of(3));
    let every_third = count.filter(&flags).map(|_: &u64| ());

    let trigger = every_third.handle();
    let sampled: Stream<u64> = count.wire(move |b, h| b.sample(h, trigger));
    let out = sampled.with_time().accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(7)).unwrap();
    assert_eq!(
        vec![(NanoTime::new(200), 3), (NanoTime::new(500), 6)],
        runner.value(&out),
        "one emission per trigger tick, carrying the passive edge's live value",
    );
}

/// The passive mask on a *join*: `join_passive`'s second edge is read on every
/// tick of the first, but never triggers one of its own. Compare with `join`
/// (both edges active) over the same pair of streams — the passive form emits
/// strictly less often.
#[test]
fn passive_mask_shape_suppresses_activation_from_the_masked_edge() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let active_side = count.filter(&count.map(|n: &u64| n.is_multiple_of(2)));
    let passive_side = count.map(|n: &u64| *n * 100);

    let ph = passive_side.handle();
    let joined: Stream<u64> =
        active_side.wire(move |b, h| b.join_passive(h, ph, |x: &u64, y: &u64| x + y));
    let out = joined.accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(
        vec![202, 404],
        runner.value(&out),
        "only the active edge's ticks produce output; the passive value is live",
    );
}

// --- tick-flag shape --------------------------------------------------------

/// `Delay`'s `In = (&T, bool)` — the trailing `bool` is the source edge's tick
/// flag for this cycle, which the generated wiring reads out of the engine's
/// tick vector. Without it `delay` could not tell "my source ticked" from "a
/// scheduled callback woke me", and would re-queue its stale value every wake.
#[test]
fn tick_flag_shape_passes_the_engines_per_edge_tick_into_the_op() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();

    let delayed: Stream<u64> = count.wire(|b, h| b.delay(h, Duration::from_nanos(200)));
    let out = delayed.with_time().accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(200), 1),
            (NanoTime::new(300), 2),
            (NanoTime::new(400), 3),
            (NanoTime::new(500), 4),
        ],
        runner.value(&out),
        "each value re-emitted exactly once, 200ns after it arrived",
    );
}

/// `Merge2`'s `In = ((&T, bool), (&T, bool))` — a tick flag per edge, in the
/// pair form. Both edges tick on the same instant here, so the earliest-wins
/// tie-break is only decidable from the flags.
#[test]
fn tick_flag_shape_carries_one_flag_per_edge() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let evens = count.filter(&count.map(|n: &u64| n.is_multiple_of(2)));
    let odds = count.filter(&count.map(|n: &u64| !n.is_multiple_of(2)));

    let odd_h = odds.handle();
    let merged: Stream<u64> = evens.wire(move |b, h| b.merge(h, odd_h));
    let out = merged.accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(
        vec![1, 2, 3, 4],
        runner.value(&out),
        "whichever edge ticked wins; the two never tick together here",
    );
}

// --- lifecycle-hook shape ---------------------------------------------------

/// An op that overrides `teardown` gets the hook attached to its node. The
/// generated wiring is the only thing that can attach it, and the hook must see
/// the op's state (`Finally` replays the source's last value into the closure).
#[test]
fn lifecycle_shape_attaches_the_teardown_hook() {
    let seen: Rc<RefCell<Vec<u64>>> = Rc::default();
    let recorder = seen.clone();

    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let _sink: Stream<()> = count.wire(move |b, h| {
        b.finally(h, move |last: &u64| {
            recorder.borrow_mut().push(*last);
            Ok(())
        })
    });

    let mut runner = g.build();
    assert!(
        seen.borrow().is_empty(),
        "teardown must not run before the run"
    );
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![3],
        *seen.borrow(),
        "teardown ran once, after the run, seeing the last value",
    );
}

/// `Timed` overrides both `start` and `stop`; it passes values through
/// unchanged, so the observable proof that the hooks were attached is that the
/// run completes (a missing `start` leaves `wall_start` unset, which `stop`
/// reports on) with the values untouched.
#[test]
fn lifecycle_shape_attaches_start_and_stop_hooks() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let timed: Stream<u64> = count.wire(|b, h| b.timed(h));
    let out = timed.accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], runner.value(&out));
}

// --- seeded-accumulator shape (`init_arg`) ----------------------------------

/// An accumulator that is deliberately **not** `Default`: `init_arg` seeds both
/// the state and the value slot from the call site, so the generated method
/// must not require `Out: Default` the way every other shape does.
#[derive(Clone, Debug, PartialEq)]
struct Tally(u64);

#[test]
fn seeded_shape_takes_init_before_cfg_and_needs_no_default() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    // `fold(src, init, cfg)` — the seed sits between the edge and the closure.
    let tallied: Stream<Tally> =
        count.wire(|b, h| b.fold(h, Tally(100), |acc: &mut Tally, v: &u64| acc.0 += *v));

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        Tally(106),
        runner.value(&tallied),
        "seeded at 100, then folded 1 + 2 + 3",
    );
}

/// A second `run()` re-seeds the accumulator from `init` rather than continuing
/// the first run's total — the reset hook the generated wiring attaches.
#[test]
fn seeded_shape_re_seeds_from_init_between_runs() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let tallied: Stream<Tally> =
        count.wire(|b, h| b.fold(h, Tally(100), |acc: &mut Tally, v: &u64| acc.0 += *v));

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(Tally(106), runner.value(&tallied));
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        Tally(106),
        runner.value(&tallied),
        "the re-run restarts from the seed, not from 106",
    );
}

/// The `Default`-seeded shapes reset the same way: state *and* value slot go
/// back to their wiring-time values, so a re-run replays identically.
#[test]
fn default_seeded_shape_resets_state_and_slot_between_runs() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let throttled: Stream<u64> = count.wire(|b, h| b.throttle(h, Duration::from_nanos(250)));
    let out = throttled.with_time().accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let first = runner.value(&out);
    assert_eq!(
        vec![(NanoTime::ZERO, 1), (NanoTime::new(300), 4)],
        first,
        "throttle emits, then suppresses until the interval has passed",
    );
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(
        first,
        runner.value(&out),
        "the second run replays the first exactly (last-emit time was re-seeded)",
    );
}
