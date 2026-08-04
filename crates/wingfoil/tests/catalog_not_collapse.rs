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

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

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
