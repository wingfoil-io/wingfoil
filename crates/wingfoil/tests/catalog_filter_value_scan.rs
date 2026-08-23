//! Node-catalog parity for the two predicate/accumulator combinators added
//! alongside `filter` and `fold`:
//!
//! * **`filter_value`** — a legacy parity gap. `NodeOperators::filter_value`
//!   has always existed on the legacy engine (`legacy/wingfoil/src/nodes/mod.rs`),
//!   and its own test comment calls it "the simpler API" against `filter`'s
//!   condition stream, but wingfoil's `StreamOps` never carried it: only
//!   the since-removed `Signal` facade, the `dynamic-graph` `Extension`, and
//!   the Python binding did.
//!   The legacy unit tests in `nodes/filter.rs` are ported verbatim below.
//!
//! * **`scan`** — new surface (no legacy twin), the value-returning twin of
//!   `fold`. Its contract is that it agrees with `fold` value-for-value and
//!   tick-for-tick, so that is what the tests assert.
//!
//! Both assert values **and** tick times, per the repo testing convention.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(100);

// --- filter_value: ported legacy parity tests -----------------------------

/// Mirrors legacy `nodes::filter::tests::passes_values_when_condition_true`:
/// `count()` produces 1..=6, keep the even ones.
#[test]
fn passes_values_when_predicate_true() {
    let g = GraphBuilder::new();
    let filtered = g
        .ticker(P)
        .count()
        .filter_value(|x: &u64| x.is_multiple_of(2))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![2, 4, 6], r.value(&filtered));
}

/// Mirrors legacy
/// `nodes::filter::tests::suppresses_all_when_condition_always_false`.
#[test]
fn suppresses_all_when_predicate_always_false() {
    let g = GraphBuilder::new();
    let filtered = g
        .ticker(P)
        .count()
        .filter_value(|_: &u64| false)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert!(r.value(&filtered).is_empty());
}

/// Tick *times*, not just values: a suppressed cycle must produce no tick at
/// all, so the surviving ticks keep the source's original instants.
#[test]
fn filter_value_preserves_source_tick_times() {
    let g = GraphBuilder::new();
    let timed = g
        .ticker(P)
        .count()
        .filter_value(|x: &u64| x.is_multiple_of(2))
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    // The ticker's first tick is at the run's start instant (t=0, count=1) —
    // see `catalog::with_time_pairs_value_with_engine_time` — so the even
    // counts land at 100 / 300 / 500.
    assert_eq!(
        vec![
            (NanoTime::new(100), 2),
            (NanoTime::new(300), 4),
            (NanoTime::new(500), 6),
        ],
        r.value(&timed)
    );
}

/// `filter_value` agrees with the `map` + `filter` spelling it replaces — the
/// desugaring legacy performs internally (`filter_value` builds a `map`ped
/// condition stream and feeds `FilterStream`).
#[test]
fn filter_value_matches_map_plus_filter() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let via_predicate = count
        .filter_value(|x: &u64| x.is_multiple_of(2))
        .accumulate();
    let cond = count.map(|x: &u64| x.is_multiple_of(2));
    let via_stream = count.filter(&cond).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(r.value(&via_stream), r.value(&via_predicate));
}

#[test]
fn filter_stays_quiet_until_source_ticks_then_samples_on_condition_ticks() {
    let g = GraphBuilder::new();
    let source = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map_filter(|i| (*i, *i >= 2));
    let condition = g.ticker(Duration::from_nanos(30)).map(|_| true);
    let values = source.filter(&condition).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(10)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(100), 2),
            (NanoTime::new(120), 2),
            (NanoTime::new(150), 2),
            (NanoTime::new(180), 2),
            (NanoTime::new(200), 3),
            (NanoTime::new(210), 3),
        ],
        r.value(&values)
    );
}

/// Shape gate, not just results: `filter_value` must cost **one** node. Legacy
/// desugars it into two (a `map` for the condition plus `FilterStream`), and
/// the results-parity test above passes either way — so assert the node count,
/// the way `merge_n.rs` does for the variadic merge.
#[test]
fn filter_value_wires_a_single_node() {
    let g = GraphBuilder::new();
    let _out = g.ticker(P).count().filter_value(|x: &u64| *x > 2);
    let baseline = {
        let g2 = GraphBuilder::new();
        let _c = g2.ticker(P).count();
        g2.build().node_count()
    };
    let r = g.build();
    assert_eq!(
        baseline + 1,
        r.node_count(),
        "filter_value should wire one node, not a map + filter pair"
    );
}

// --- scan -----------------------------------------------------------------

/// The running total a newcomer reaches for, spelled the `Iterator::fold` way.
#[test]
fn scan_accumulates_returning_values() {
    let g = GraphBuilder::new();
    let running = g
        .ticker(P)
        .count()
        .scan(0u64, |acc: &u64, v: &u64| acc + v)
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 3, 6, 10, 15], r.value(&running));
}

/// `scan` and `fold` are the same op with the closure spelled differently:
/// identical values at identical instants.
#[test]
fn scan_agrees_with_fold_on_values_and_times() {
    let g = GraphBuilder::new();
    let count = g.ticker(P).count();
    let scanned = count
        .scan(0u64, |acc: &u64, v: &u64| acc + v)
        .with_time()
        .accumulate();
    let folded = count
        .fold(0u64, |acc: &mut u64, v: &u64| *acc += v)
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
    assert_eq!(r.value(&folded), r.value(&scanned));
    // And pin the absolute expectation, so a shared regression cannot make
    // both sides agree on the wrong thing.
    assert_eq!(
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(100), 3),
            (NanoTime::new(200), 6),
            (NanoTime::new(300), 10),
            (NanoTime::new(400), 15),
            (NanoTime::new(500), 21),
            (NanoTime::new(600), 28),
            (NanoTime::new(700), 36),
        ],
        r.value(&scanned)
    );
}

/// The accumulator need not be the input type, and need not be `Default` —
/// the seed comes from the call site (the `init_arg` shape `fold` uses).
#[test]
fn scan_accumulator_may_differ_from_input() {
    let g = GraphBuilder::new();
    let labels = g
        .ticker(P)
        .count()
        .scan(String::from("start"), |acc: &String, v: &u64| {
            format!("{acc}-{v}")
        })
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec!["start-1", "start-1-2", "start-1-2-3"],
        r.value(&labels)
    );
}

/// A re-run re-seeds the accumulator from the wiring-time init, rather than
/// continuing from where the previous run left off.
#[test]
fn scan_reseeds_on_rerun() {
    let g = GraphBuilder::new();
    let running = g.ticker(P).count().scan(0u64, |acc: &u64, v: &u64| acc + v);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(15, r.value(&running));
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(15, r.value(&running));
}
