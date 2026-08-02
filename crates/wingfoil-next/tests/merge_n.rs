//! The n-ary merge (`MergeN` / `Builder::merge_n`), which backs `merge_all`
//! and `fan`.
//!
//! `merge_all` used to left-fold into a chain of `n - 1` binary `merge`s. That
//! is *semantically* identical — 2-ary merge's earliest-wins tie-break is
//! associative — which is exactly why it survived: every results-parity test
//! passed. What it cost was `n - 1` extra nodes and `n - 1` extra depth, and
//! on a busy 256-wide fan-in that measured **1.86x** legacy wingfoil's single
//! `merge(vec)` node — a Phase 6 gate violation
//! (`next-interpreted >= legacy-interpreted`).
//!
//! So these tests come in two halves, and the first is the one that would have
//! caught it:
//!
//! * **Shape** — `merge_all(k inputs)` wires exactly one node
//!   (`wide_merge_costs_one_node`, `fan_costs_one_merge_node`). Asserted on
//!   `Runner::node_count()`, so it is a pass/fail gate in CI rather than a
//!   benchmark reading.
//! * **Semantics** — the tie-break, burst delivery, the empty case, and
//!   three-engine agreement, i.e. everything that was *already* right and must
//!   stay so through the rewrite.

use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A `k`-way `merge_all` costs **one** merge node, whatever `k` is.
///
/// The graph is `k` branches (3 nodes each: ticker + count + map) fanned into
/// one merge, plus an `accumulate` to read it — so the expected total is
/// `3k + 1 (merge) + 1 (accumulate)`. Under the old chain it was
/// `3k + (k - 1) + 1`: at `k = 64`, **194 nodes rather than 256**.
#[test]
fn wide_merge_costs_one_node() {
    const K: usize = 64;

    let g = GraphBuilder::new();
    let branches: Vec<Stream<u64>> = (0..K)
        .map(|i| {
            g.ticker(Duration::from_nanos(10 + i as u64))
                .count()
                .map(move |c| *c * 1000 + i as u64)
        })
        .collect();
    let rest: Vec<&Stream<u64>> = branches[1..].iter().collect();
    let out = branches[0].merge_all(&rest).accumulate();
    let runner = g.build();

    assert_eq!(
        runner.node_count(),
        3 * K + 2,
        "a {K}-way merge_all is one merge node ({} under the old binary chain)",
        3 * K + (K - 1) + 1
    );
    // The handle is live wiring, not decoration — keep it used so the shape
    // under test is the shape that would actually run.
    let _ = out;
}

/// Same gate for `fan`, whose fan-in is the one that made the depth term
/// visible in the first place: an `n`-way fan is `n` branch tails plus one
/// merge, so its *depth* no longer grows with `n` either.
#[test]
fn fan_costs_one_merge_node() {
    const N: usize = 32;

    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(10)).count();
    let fanned = src.fan(N, |s| s.map(|c| *c + 1));
    let out = fanned.accumulate();
    let runner = g.build();

    // ticker + count + N branch maps + 1 merge + accumulate.
    assert_eq!(
        runner.node_count(),
        2 + N + 2,
        "a {N}-way fan is one merge node, not a {}-deep chain",
        N - 1
    );
    let _ = out;
}

/// A single-branch `fan` needs no merge at all — it is just the branch.
#[test]
fn single_branch_fan_wires_no_merge() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(10)).count();
    let out = src.fan(1, |s| s.map(|c| *c + 1)).accumulate();
    let runner = g.build();

    // ticker + count + the one branch map + accumulate — no merge.
    assert_eq!(runner.node_count(), 4);
    let _ = out;
}

/// `merge_all(&[])` is the receiver itself, wiring nothing.
#[test]
fn empty_merge_all_is_the_receiver() {
    let g = GraphBuilder::new();
    let lone = g.ticker(Duration::from_nanos(10)).count();
    let merged = lone.merge_all(&[]);

    assert_eq!(
        merged.handle().index(),
        lone.handle().index(),
        "merge_all(&[]) returns the same node, not a new one"
    );
    assert_eq!(g.build().node_count(), 2, "ticker + count, no merge node");
}

/// The tie-break, with **every** input ticking in the same cycle: the
/// earliest-supplied one wins, in `[receiver, others…]` order.
///
/// Four tickers of the same period tick together on every cycle, so the
/// tie-break arm runs every cycle and only the receiver's values may appear.
/// A merge that picked the last ticked input (or iterated its upstreams in
/// registration order rather than supplied order) fails here.
#[test]
fn n_ary_tie_break_takes_the_earliest_supplied() {
    let g = GraphBuilder::new();
    let period = Duration::from_nanos(10);
    let a = g.ticker(period).count();
    let b = g.ticker(period).count().map(|c| *c + 100);
    let c = g.ticker(period).count().map(|c| *c + 200);
    let d = g.ticker(period).count().map(|c| *c + 300);

    let out = a.merge_all(&[&b, &c, &d]).accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();

    assert_eq!(runner.value(&out), vec![1, 2, 3, 4]);
}

/// When the earliest-supplied input is *quiet*, the next ticked one wins —
/// the arm the same-period test above can never reach.
///
/// `a` ticks every 30ns, `b` every 10ns, so on the cycles where both are due
/// `a` wins and on the others `b` does.
#[test]
fn quiet_inputs_are_skipped_in_supplied_order() {
    let g = GraphBuilder::new();
    let a = g.ticker(Duration::from_nanos(30)).count().map(|c| *c + 100);
    let b = g.ticker(Duration::from_nanos(10)).count().map(|c| *c + 200);

    let out = a.merge_all(&[&b]).with_time().accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    // Both tickers are due at t=0 and t=30 (`a` wins those); `b` alone at
    // 10, 20, 40, 50.
    assert_eq!(
        runner.value(&out),
        vec![
            (NanoTime::new(0), 101),
            (NanoTime::new(10), 202),
            (NanoTime::new(20), 203),
            (NanoTime::new(30), 102),
            (NanoTime::new(40), 205),
            (NanoTime::new(50), 206),
        ]
    );
}

/// The n-ary node is a drop-in for the chain it replaced: the same inputs
/// produce the same series either way. This is the oracle the shape gates
/// above cannot be: it pins that removing `n - 1` nodes removed no behaviour.
#[test]
fn n_ary_merge_matches_the_binary_chain() {
    fn drivers(g: &GraphBuilder) -> (Stream<u64>, Stream<u64>, Stream<u64>) {
        let a = g.ticker(Duration::from_nanos(30)).count();
        let b = g.ticker(Duration::from_nanos(10)).count().map(|c| *c + 100);
        let c = g.ticker(Duration::from_nanos(20)).count().map(|c| *c + 200);
        (a, b, c)
    }

    let g1 = GraphBuilder::new();
    let (a, b, c) = drivers(&g1);
    let n_ary = a.merge_all(&[&b, &c]).accumulate();
    let mut r1 = g1.build();
    r1.run(HISTORICAL, RunFor::Cycles(12)).unwrap();

    let g2 = GraphBuilder::new();
    let (a, b, c) = drivers(&g2);
    let chained = a.merge(&b).merge(&c).accumulate();
    let mut r2 = g2.build();
    r2.run(HISTORICAL, RunFor::Cycles(12)).unwrap();

    assert!(!r1.value(&n_ary).is_empty());
    assert_eq!(
        r1.value(&n_ary),
        r2.value(&chained),
        "the n-ary node and the binary chain it replaced agree"
    );
}

/// Burst inputs ride through the merge whole — the earliest-supplied ticked
/// input's burst is forwarded, never flattened or latest-wins collapsed.
#[test]
fn merge_all_forwards_bursts_intact() {
    let g = GraphBuilder::new();
    let a = g.replay_results(vec![
        Ok((1u32, NanoTime::new(10))),
        Ok((2, NanoTime::new(10))),
        Ok((3, NanoTime::new(30))),
    ]);
    let b = g.replay_results(vec![
        Ok((10u32, NanoTime::new(20))),
        Ok((11, NanoTime::new(20))),
    ]);

    let out = a.merge_all(&[&b]).with_time().accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Forever).unwrap();

    let got: Vec<(NanoTime, Vec<u32>)> = runner
        .value(&out)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        got,
        vec![
            (NanoTime::new(10), vec![1, 2]),
            (NanoTime::new(20), vec![10, 11]),
            (NanoTime::new(30), vec![3]),
        ],
        "same-instant values stay grouped in one burst through the merge"
    );
}

// ---- nitro! / compiled / nested coverage ----------------------------------
//
// Both fan-in spellings now emit a single n-ary node on the compiled paths
// too, so each block is also the two-sided registration guard `op_completeness`
// describes: it only compiles if the fluent method and the `merge_n`
// forwarders both exist.

wingfoil_next::nitro! {
    fn merged(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(std::time::Duration::from_nanos(30)).count();
        let b = g.ticker(std::time::Duration::from_nanos(10)).count().map(|c| *c + 100);
        let c = g.ticker(std::time::Duration::from_nanos(20)).count().map(|c| *c + 200);
        let out = a.merge_all(&[&b, &c]).accumulate();
        out
    }
}

wingfoil_next::nitro! {
    fn fanned(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let src = g.ticker(std::time::Duration::from_nanos(10)).count();
        let out = src.fan(4, |s| s.map(|c| *c + 1)).accumulate();
        out
    }
}

/// `merge_all` inside `nitro!` — new capability: it used to be fluent-only
/// ("sugar over a primitive — spell the primitive"), because a chain was all
/// it was. Now it is a real op with real forwarders, so all three engines
/// emit it and must agree.
#[test]
fn graph_merge_all_agrees_across_engines() {
    let run_for = RunFor::Cycles(12);

    let (mut runner, out) = merged::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);

    let (compiled,) = merged::compiled(HISTORICAL, run_for).unwrap();

    let g = GraphBuilder::new();
    let island = merged::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();

    assert!(!interpreted.is_empty());
    assert_eq!(interpreted, compiled, "interpreted == compiled");
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
}

/// `fan` inside `nitro!` emits one n-ary merge on the compiled paths, matching
/// the fluent wiring it is derived from.
#[test]
fn graph_fan_agrees_across_engines() {
    let run_for = RunFor::Cycles(8);

    let (mut runner, out) = fanned::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);

    let (compiled,) = fanned::compiled(HISTORICAL, run_for).unwrap();

    let g = GraphBuilder::new();
    let island = fanned::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();

    assert!(!interpreted.is_empty());
    assert_eq!(interpreted, compiled, "interpreted == compiled");
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
}
