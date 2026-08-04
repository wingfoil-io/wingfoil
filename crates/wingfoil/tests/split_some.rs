//! `Stream::split_some` — two independently-ticking outputs from one node.
//!
//! An `Op` has a single `Out`, so a node emitting two things at different rates
//! emits one value describing both and fans out at wiring time. These tests pin
//! that the two branches tick *separately* (the whole point), that the shared
//! upstream is still computed once, and that the branches recombine
//! glitch-free like any other stream.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);

/// Each branch ticks only on the cycles where its own side is `Some`, and the
/// ticks land at the source's times — not renumbered by the filtering.
#[test]
fn each_branch_ticks_only_when_its_own_side_is_some() {
    let g = GraphBuilder::new();
    let (evens, thirds) = g
        .ticker(STEP)
        .count()
        .map(|n: &u64| {
            (
                n.is_multiple_of(2).then_some(*n),
                n.is_multiple_of(3).then_some(n * 100),
            )
        })
        .split_some();

    let evens = evens.with_time().accumulate();
    let thirds = thirds.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).expect("run completes");

    // Counts 1..=6 at t = 0, 100, .. 500.
    assert_eq!(
        vec![
            (NanoTime::new(100), 2),
            (NanoTime::new(300), 4),
            (NanoTime::new(500), 6),
        ],
        r.value(&evens)
    );
    assert_eq!(
        vec![(NanoTime::new(200), 300), (NanoTime::new(500), 600)],
        r.value(&thirds)
    );
}

/// A branch that is `None` for the whole run never ticks at all — it does not
/// emit `Default` values, which is what a plain `split()` of an `Option` pair
/// would do.
#[test]
fn a_branch_that_never_fires_stays_completely_quiet() {
    let g = GraphBuilder::new();
    let (never, always) = g
        .ticker(STEP)
        .count()
        .map(|n: &u64| (None::<u64>, Some(*n)))
        .split_some();

    let never = never.accumulate();
    let always = always.accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).expect("run completes");

    assert!(
        r.value(&never).is_empty(),
        "the quiet branch emitted values"
    );
    assert_eq!(vec![1, 2, 3, 4], r.value(&always));
}

/// The shared upstream is one node, cycled once per tick however many branches
/// read it — the property that makes this cheaper than computing the book twice.
#[test]
fn the_shared_upstream_is_evaluated_once_per_tick() {
    let evaluations = Rc::new(RefCell::new(0usize));
    let counter = evaluations.clone();

    let g = GraphBuilder::new();
    let (a, b) = g
        .ticker(STEP)
        .count()
        .map(move |n: &u64| {
            *counter.borrow_mut() += 1;
            (Some(*n), Some(*n))
        })
        .split_some();

    let a = a.accumulate();
    let b = b.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).expect("run completes");

    assert_eq!(vec![1, 2, 3, 4, 5], r.value(&a));
    assert_eq!(vec![1, 2, 3, 4, 5], r.value(&b));
    assert_eq!(
        5,
        *evaluations.borrow(),
        "the shared node was evaluated more than once per tick"
    );
}

/// Downstream of the split each side is an ordinary stream: recombining them
/// fires **once** per cycle, after both, like any other branch-and-recombine.
#[test]
fn the_two_branches_recombine_glitch_free() {
    let fires = Rc::new(RefCell::new(0usize));
    let counter = fires.clone();

    let g = GraphBuilder::new();
    let (a, b) = g
        .ticker(STEP)
        .count()
        .map(|n: &u64| (Some(*n), Some(n * 2)))
        .split_some();

    let combined = a
        .join(&b, move |x: &u64, y: &u64| {
            *counter.borrow_mut() += 1;
            x + y
        })
        .with_time()
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).expect("run completes");

    assert_eq!(
        vec![
            (NanoTime::new(0), 3),
            (NanoTime::new(100), 6),
            (NanoTime::new(200), 9),
            (NanoTime::new(300), 12),
        ],
        r.value(&combined)
    );
    assert_eq!(
        4,
        *fires.borrow(),
        "the recombine fired more than once per cycle — a glitch"
    );
}

/// The plain `split()` is unchanged: both branches tick together, on every
/// source tick. Keeping the contrast pinned is what stops the two being
/// confused for one another.
#[test]
fn plain_split_still_ticks_both_branches_together() {
    let g = GraphBuilder::new();
    let (a, b) = g.ticker(STEP).count().map(|n: &u64| (*n, n * 2)).split();

    let a = a.accumulate();
    let b = b.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).expect("run completes");

    assert_eq!(vec![1, 2, 3], r.value(&a));
    assert_eq!(vec![2, 4, 6], r.value(&b));
}
