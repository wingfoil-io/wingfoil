//! Node-catalog parity for `not` and `collapse` after their promotion from
//! fluent-only sugar to real ops (`ops::Not` / `ops::Collapse`).
//!
//! The promotion is a behaviour-preserving refactor, so the contract these
//! tests pin is **equivalence with the desugarings they replace**:
//!
//! * `not` was `map(|v| !v.clone())`;
//! * `collapse` was `map_filter(|x| match x.clone().into_iter().last() {
//!   Some(v) => (v, true), None => (Default::default(), false) })`.
//!
//! `collapse`'s quiet-on-empty rule is the part worth guarding: it is a real
//! tick-suppression contract, not just a value mapping, so the tests assert
//! tick *times* and not merely values.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{Burst, NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(100);

// --- not ------------------------------------------------------------------

#[test]
fn not_negates_each_value() {
    let g = GraphBuilder::new();
    let flags = g
        .ticker(P)
        .count()
        .map(|i: &u64| i.is_multiple_of(2))
        .not()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // count = 1,2,3,4 -> is_even = f,t,f,t -> not = t,f,t,f
    assert_eq!(vec![true, false, true, false], r.value(&flags));
}

/// Equivalence with the `map` desugar it replaces, values and tick times.
#[test]
fn not_matches_map_desugar() {
    let g = GraphBuilder::new();
    let flags = g.ticker(P).count().map(|i: &u64| i.is_multiple_of(2));
    let via_op = flags.not().with_time().accumulate();
    let via_map = flags.map(|b: &bool| !*b).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(r.value(&via_map), r.value(&via_op));
}

/// `not` is generic over `std::ops::Not`, not hard-wired to `bool`.
#[test]
fn not_works_on_integer_bitwise_negation() {
    let g = GraphBuilder::new();
    let bits = g.ticker(P).count().map(|i: &u64| *i as i64).not();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(!3i64, r.value(&bits));
}

// --- collapse -------------------------------------------------------------

#[test]
fn collapse_emits_last_item() {
    let g = GraphBuilder::new();
    let last = g
        .ticker(P)
        .count()
        .map(|i: &u64| vec![*i, *i * 10])
        .collapse()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![10, 20, 30], r.value(&last));
}

/// The tick-suppression contract: an empty iterable emits **nothing**, so the
/// surviving ticks keep the source's instants rather than shifting.
#[test]
fn collapse_is_quiet_on_empty() {
    let g = GraphBuilder::new();
    let timed = g
        .ticker(P)
        .count()
        // Odd counts carry a value, even counts are empty.
        .map(|i: &u64| {
            if i.is_multiple_of(2) {
                Vec::new()
            } else {
                vec![*i]
            }
        })
        .collapse()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    // count = 1..6 at t = 0,100,..,500; only the odd counts tick.
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(200), 3),
            (NanoTime::new(400), 5),
        ],
        r.value(&timed)
    );
}

/// Equivalence with the `map_filter` desugar it replaces — including the
/// suppressed cycles, which is what a values-only check would miss.
#[test]
fn collapse_matches_map_filter_desugar() {
    let g = GraphBuilder::new();
    let lists = g.ticker(P).count().map(|i: &u64| {
        if i.is_multiple_of(3) {
            Vec::new()
        } else {
            vec![*i, *i + 1]
        }
    });
    let via_op = lists.collapse().with_time().accumulate();
    let via_desugar = lists
        .map_filter(|x: &Vec<u64>| match x.clone().into_iter().last() {
            Some(last) => (last, true),
            None => (0u64, false),
        })
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();
    assert_eq!(r.value(&via_desugar), r.value(&via_op));
}

/// `collapse` is generic over `IntoIterator`, so a `Burst` payload — the shape
/// every burst source produces — collapses too.
#[test]
fn collapse_works_on_a_burst() {
    let g = GraphBuilder::new();
    let (stream, sender) = g.channel::<u64>();
    sender.send_at(1, NanoTime::new(0));
    sender.send_at(2, NanoTime::new(0));
    sender.send_at(3, NanoTime::new(100));
    sender.close();
    let last = stream.collapse().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // Same-instant values ride one burst; collapse takes its last item.
    assert_eq!(vec![2, 3], r.value(&last));
}

// --- collapse: borrow-iteration (#824) ------------------------------------
//
// `Collapse::cycle` iterates a **borrow** of its input
// (`for<'b> &'b T: IntoIterator<Item = &'b OUT>`) and clones only the item it
// emits, where it used to clone the whole container and throw all but the last
// element away. The tests below pin the two halves of that:
//
// * behaviour is unchanged for both container shapes — `Burst<T>` (the ingest
//   shape, a `TinyVec<[T; 1]>`) and `Vec<T>` — at single-item, multi-item and
//   empty payloads, on **all three engines**. The bound has to be threaded
//   through the `#[op]`-generated builder/fluent/`nitro!` forwarders as well as
//   `cycle`, and a forwarder-bound mistake shows up in the compiled tiers, not
//   the interpreted one;
// * the clone count itself, which is the point of the change.

/// Assert `interpreted() == compiled() == nested()` for a self-contained
/// (source) graph module whose single output is an accumulated sequence — the
/// three-engine pattern from `compiled_stateful_ops.rs`. The nested check
/// mounts the graph as a source island in an outer interpreted graph.
macro_rules! assert_three_engines {
    ($module:ident, $run_for:expr, $expected:expr) => {{
        let run_for = $run_for;

        let (mut runner, out) = $module::interpreted();
        runner.run(HISTORICAL, run_for).unwrap();
        let interpreted = runner.value(out);
        assert_eq!($expected, interpreted, "interpreted value mismatch");

        let (compiled,) = $module::compiled(HISTORICAL, run_for).unwrap();
        assert_eq!(interpreted, compiled, "compiled must match interpreted");

        let g = GraphBuilder::new();
        let island = $module::nested(&g);
        let mut r = g.build();
        r.run(HISTORICAL, run_for).unwrap();
        assert_eq!(
            interpreted,
            r.value(&island),
            "nested island must match interpreted"
        );
    }};
}

wingfoil::nitro! {
    fn collapse_vec_timed(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .ticker(P)
            .count()
            // count % 3: 0 -> empty, 1 -> one item, 2 -> three items.
            .map(|i: &u64| match i % 3 {
                0 => Vec::new(),
                1 => vec![*i * 10],
                _ => vec![*i * 10, *i * 10 + 1, *i * 10 + 2],
            })
            .collapse()
            .with_time()
            .accumulate();
        acc
    }
}

wingfoil::nitro! {
    fn collapse_burst_timed(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .ticker(P)
            .count()
            // The identical payloads as `collapse_vec_timed`, burst-shaped:
            // empty, inline (one item, no spill), spilled (three items).
            .map(|i: &u64| -> Burst<u64> {
                match i % 3 {
                    0 => wingfoil::burst![],
                    1 => wingfoil::burst![*i * 10],
                    _ => wingfoil::burst![*i * 10, *i * 10 + 1, *i * 10 + 2],
                }
            })
            .collapse()
            .with_time()
            .accumulate();
        acc
    }
}

/// The counter ticks 1..6 at t = 0,100,…,500. Counts 3 and 6 carry an empty
/// container and are suppressed; the rest emit their **last** item at the
/// source's own instant.
fn collapse_expected() -> Vec<(NanoTime, u64)> {
    vec![
        (NanoTime::new(0), 10),   // count 1, one item  -> 10
        (NanoTime::new(100), 22), // count 2, three     -> 22
        (NanoTime::new(300), 40), // count 4, one item  -> 40
        (NanoTime::new(400), 52), // count 5, three     -> 52
    ]
}

#[test]
fn collapse_on_vec_agrees_across_engines() {
    assert_three_engines!(collapse_vec_timed, RunFor::Cycles(6), collapse_expected());
}

#[test]
fn collapse_on_burst_agrees_across_engines() {
    assert_three_engines!(collapse_burst_timed, RunFor::Cycles(6), collapse_expected());
}

/// `Burst` and `Vec` are not merely each correct — they are *identical*, item
/// for item and instant for instant, over the same payloads.
#[test]
fn collapse_agrees_between_burst_and_vec() {
    let (mut vr, vout) = collapse_vec_timed::interpreted();
    vr.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let (mut br, bout) = collapse_burst_timed::interpreted();
    br.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vr.value(vout), br.value(bout));
}

/// A payload that counts its own clones, so "one clone per tick, not one per
/// item" is a pinned fact rather than a claim about speed.
///
/// Only this test constructs a `Counted`, so the process-wide counter is not
/// shared with the parallel tests around it.
#[derive(Debug, Default, PartialEq)]
struct Counted(u64);

static CLONES: AtomicUsize = AtomicUsize::new(0);

impl Clone for Counted {
    fn clone(&self) -> Self {
        CLONES.fetch_add(1, Ordering::Relaxed);
        Self(self.0)
    }
}

/// `collapse` clones the **one** item it emits, not the whole container: four
/// items per tick over three ticks is three clones, where cloning the input
/// first would be twelve.
#[test]
fn collapse_clones_only_the_item_it_emits() {
    let g = GraphBuilder::new();
    let last = g
        .ticker(P)
        .count()
        // Four freshly *constructed* (not cloned) items per tick.
        .map(|i: &u64| {
            vec![
                Counted(*i),
                Counted(*i * 10),
                Counted(*i * 100),
                Counted(*i * 1000),
            ]
        })
        .collapse()
        // Read the payload out by reference so the tail adds no clones of its
        // own — `accumulate` on a `Counted` would.
        .map(|c: &Counted| c.0)
        .accumulate();
    let mut r = g.build();
    CLONES.store(0, Ordering::Relaxed);
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1000, 2000, 3000], r.value(&last));
    assert_eq!(3, CLONES.load(Ordering::Relaxed));
}
