//! `combine` across all three engines (`CombineN` / `Builder::combine`).
//!
//! `combine` is the burst-shaped twin of the n-ary merge: where `merge_all`
//! picks **one** winner from the inputs that ticked this instant, `combine`
//! keeps **all** of them, in supplied order. That is the burst pattern the
//! engine is built around — same-instant values ride one burst, never
//! latest-wins, never dropped — so having it work on only one of the three
//! engines was the last real hole in the fan-in vocabulary.
//!
//! It was fluent-only for the same reason `merge_all` once was: a variadic
//! fan-in has no fixed-arity tuple `In` for `#[op]` to parse. `merge_n` showed
//! the route (a slice `In` plus hand-written forwarders) and this follows it.
//!
//! Two things these tests pin that a values-only check would not:
//!
//! * **The tick mask.** A burst carries only the inputs that ticked this
//!   instant. A `combine` that gathered all of them unconditionally would
//!   still agree across the three engines, so that check reads values rather
//!   than comparing engines.
//! * **The spelling.** `combine` lives on the builder rather than on a stream,
//!   because no input is privileged. `nitro!` therefore accepts it in
//!   builder-root position — the one call there that is not a source — so the
//!   macro spelling is the fluent spelling rather than a second name for one
//!   op.

use std::time::Duration;

use wingfoil::op::Tick;
use wingfoil::ops::CombineN;
use wingfoil::prelude::*;
use wingfoil::{Burst, NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

fn burst_of(items: &[u64]) -> Burst<u64> {
    let mut b = Burst::new();
    for &i in items {
        b.push(i);
    }
    b
}

// ---- nitro! / compiled / nested coverage ----------------------------------
//
// This block is also the two-sided registration guard: it only compiles if
// both the fluent `GraphBuilder::combine` and the `__wf_op_combine_*`
// forwarders exist.

wingfoil::nitro! {
    fn combined(g: &GraphBuilder) -> Stream<Vec<Burst<u64>>> {
        let a = g.ticker(std::time::Duration::from_nanos(10)).count();
        let b = g.ticker(std::time::Duration::from_nanos(20)).count().map(|c| *c + 100);
        let c = g.ticker(std::time::Duration::from_nanos(30)).count().map(|c| *c + 200);
        let out = g.combine(&[a, b, c]).accumulate();
        out
    }
}

/// The three tickers deliberately have *different* periods, so the graph
/// visits every arity: instants where one, two or all three inputs tick. A
/// same-period fan-in would exercise only the all-ticked case and would pass
/// even if the tick mask were ignored entirely.
#[test]
fn combine_agrees_across_engines() {
    let run_for = RunFor::Cycles(12);

    let (mut runner, out) = combined::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);

    let (compiled,) = combined::compiled(HISTORICAL, run_for).unwrap();

    let g = GraphBuilder::new();
    let island = combined::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();

    assert!(!interpreted.is_empty(), "combine produced no bursts");
    assert!(
        interpreted.iter().any(|b| b.len() > 1),
        "no instant gathered more than one value — the fan-in is untested"
    );
    assert!(
        interpreted.iter().any(|b| b.len() == 1),
        "no instant gathered exactly one value — the tick mask is untested"
    );
    assert_eq!(interpreted, compiled, "interpreted == compiled");
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
}

/// The values themselves, against a hand-computed expectation rather than
/// against another engine — so all three agreeing on the *wrong* answer would
/// still fail. Mirrors `catalog_flow::combine_gathers_same_instant_values`,
/// which covers the same graph fluently.
#[test]
fn combine_gathers_in_supplied_order_on_the_compiled_path() {
    wingfoil::nitro! {
        fn same_rate(g: &GraphBuilder) -> Stream<Vec<Burst<u64>>> {
            let src = g.ticker(std::time::Duration::from_nanos(10)).count();
            let tens = src.map(|c| *c * 10);
            let hundreds = src.map(|c| *c * 100);
            let out = g.combine(&[src, tens, hundreds]).accumulate();
            out
        }
    }

    let (compiled,) = same_rate::compiled(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            burst_of(&[1, 10, 100]),
            burst_of(&[2, 20, 200]),
            burst_of(&[3, 30, 300]),
        ],
        compiled
    );
}

/// A burst carries exactly the inputs that ticked *this* instant — the others
/// are absent, not stale.
///
/// The two tickers share every third instant, so the run contains bursts of
/// one and of two. Reading the values (rather than comparing engines) is what
/// makes this a check on the tick mask: a `combine` that gathered every input
/// unconditionally would produce two-element bursts throughout and still agree
/// across all three engines.
#[test]
fn combine_gathers_only_the_inputs_that_ticked() {
    let g = GraphBuilder::new();
    let fast = g.ticker(Duration::from_nanos(10)).count();
    let slow = g.ticker(Duration::from_nanos(30)).count().map(|c| *c + 100);
    let acc = g.combine(&[fast, slow]).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(
        vec![
            burst_of(&[1, 101]), // t=0:  both tickers fire
            burst_of(&[2]),      // t=10: fast only
            burst_of(&[3]),      // t=20: fast only
            burst_of(&[4, 102]), // t=30: both again
            burst_of(&[5]),
            burst_of(&[6]),
        ],
        r.value(&acc)
    );
}

/// The empty-burst rule, tested directly on [`CombineN::emit`] rather than
/// through a graph — because **no graph can reach it**. A node is cycled only
/// when one of its active upstreams ticked, on every engine, so `combine`
/// with nothing to gather is not a state the scheduler produces. (My first
/// attempt at this test asserted it *was* reachable and was simply wrong: the
/// combine node never ran on the quiet instants at all.)
///
/// The branch still earns its keep. It is the one piece of semantics the
/// interpreted path and `Op::cycle` implement separately — the interpreted
/// wiring cannot call `cycle` without materialising a borrow guard per
/// upstream — so routing both through `emit` is what stops them drifting if a
/// future activation shape (a shared scheduled wake, a passive edge) ever does
/// reach it. Pinning it here says which answer they must agree on.
#[test]
fn an_empty_gather_is_quiet_not_an_empty_burst() {
    assert!(matches!(CombineN::<u64>::emit(Burst::new()), Tick::Quiet));
    assert!(matches!(
        CombineN::<u64>::emit(burst_of(&[7])),
        Tick::Value(_)
    ));
}

/// `combine` wires exactly **one** node whatever its width — the shape claim,
/// asserted like `merge_n`'s rather than left to a benchmark.
///
/// `k` branches of 3 nodes each (ticker + count + map), plus the combine node
/// and the accumulate that reads it.
#[test]
fn wide_combine_costs_one_node() {
    const K: usize = 32;

    let g = GraphBuilder::new();
    let branches: Vec<Stream<u64>> = (0..K)
        .map(|i| {
            g.ticker(Duration::from_nanos(10 + i as u64))
                .count()
                .map(move |c| *c * 1000 + i as u64)
        })
        .collect();
    let out = g.combine(&branches).accumulate();
    let runner = g.build();

    assert_eq!(
        runner.node_count(),
        3 * K + 2,
        "a {K}-way combine is one node"
    );
    let _ = out;
}
