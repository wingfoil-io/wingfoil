//! Parity tests for the public custom-node primitive
//! ([`GraphBuilder::custom_node`]) — the next equivalent of legacy
//! `MutableNode` + `StreamPeekRef`, and the extension point the wingfoil-python
//! `PyProxyStream` (a Python object acting as a graph node) is rewired onto.
//!
//! A custom node declares arbitrary active/passive upstream edges and a cycle
//! closure producing a [`Tick`]. These tests assert it reproduces the
//! observable behaviour (values **and** tick times) of the equivalent
//! combinator graph — `map`, `map_filter`, and `sample` — so the primitive is
//! a faithful twin of the legacy node path it replaces.

use std::rc::Rc;
use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A custom node reading its active upstream reproduces `map`, tick-for-tick.
/// It reads the upstream value through a [`Stream::value_slot`] captured at
/// wiring time — the next analogue of a legacy `MutableNode` reaching its
/// upstream `Rc<dyn Stream>`. Asserts values *and* tick times against a `map`
/// twin wired into the same graph.
#[test]
fn custom_node_reading_upstream_matches_map() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // 1,2,3,4

    // Reference: the same transform expressed as a `map`.
    let reference = count.map(|i| i * 10).with_time().accumulate();

    // Custom node doing the same, reading the upstream slot itself.
    let src = count.value_slot();
    let custom: Stream<u64> =
        g.custom_node(&[count.upstream()], &[], Activation::NONE, move |_ctx| {
            Ok(Tick::Value(*src.borrow() * 10))
        });
    let got = custom.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();

    // Same values at the same tick times as the `map` twin.
    assert_eq!(r.value(&reference), r.value(&got));
}

/// A custom node returning `Tick::Quiet` on some cycles suppresses the tick —
/// the legacy `cycle() -> bool` "did I tick?" shape the Python proxy uses.
/// Reproduces `map_filter` keeping only even counts.
#[test]
fn custom_node_quiet_suppresses_tick_like_map_filter() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // 1..=6

    let reference = count
        .map_filter(|i| (*i, i.is_multiple_of(2)))
        .with_time()
        .accumulate();

    let src = count.value_slot();
    let evens: Stream<u64> = g.custom_node(&[count.upstream()], &[], Activation::NONE, move |_| {
        let v = *src.borrow();
        Ok(if v.is_multiple_of(2) {
            Tick::Value(v)
        } else {
            Tick::Quiet
        })
    });
    let got = evens.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(vec![2, 4, 6], evens_values(&r, &got));
    assert_eq!(r.value(&reference), r.value(&got));
}

/// A custom node active on a slow trigger but *passively* reading a fast
/// counter reproduces `sample`: the passive upstream is read at the active
/// upstream's instants without triggering the node itself.
#[test]
fn custom_node_passive_upstream_matches_sample() {
    let g = GraphBuilder::new();
    let fast = g.ticker(Duration::from_nanos(10)).count(); // 1,2,3,... every 10ns
    let slow = g.ticker(Duration::from_nanos(30)); // unit trigger every 30ns

    // Reference: sample the fast counter on the slow trigger.
    let reference = fast.sample(&slow).with_time().accumulate();

    // Custom node: active on `slow`, passively reads `fast`'s current value.
    let fast_slot = fast.value_slot();
    let sampled: Stream<u64> = g.custom_node(
        &[slow.upstream()],
        &[fast.upstream()],
        Activation::NONE,
        move |_| Ok(Tick::Value(*fast_slot.borrow())),
    );
    let got = sampled.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(9)).unwrap();

    assert_eq!(r.value(&reference), r.value(&got));
}

/// A custom node may own arbitrary external state (as the Python proxy owns its
/// Python object) rather than reading engine slots — here an accumulator held
/// in a captured `Rc<RefCell>`, reproducing `fold`.
#[test]
fn custom_node_with_owned_state_matches_fold() {
    use std::cell::RefCell;

    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count(); // 1,2,3,4

    let reference = count.fold(0u64, |acc, v| *acc += v).accumulate();

    let src = count.value_slot();
    let running = Rc::new(RefCell::new(0u64));
    let sum: Stream<u64> = g.custom_node(&[count.upstream()], &[], Activation::NONE, move |_| {
        let mut total = running.borrow_mut();
        *total += *src.borrow();
        Ok(Tick::Value(*total))
    });
    let got = sum.accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();

    assert_eq!(vec![1, 3, 6, 10], r.value(&got));
    assert_eq!(r.value(&reference), r.value(&got));
}

/// A graph containing a custom node is single-run in v1 (caller-owned state has
/// no engine reset hook) — a second `run` errors rather than continuing stale
/// state, the same contract as a mounted island / external source.
#[test]
fn custom_node_graph_is_single_run() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let src = count.value_slot();
    let c: Stream<u64> = g.custom_node(&[count.upstream()], &[], Activation::NONE, move |_| {
        Ok(Tick::Value(*src.borrow()))
    });
    let _acc = c.accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert!(
        r.run(HISTORICAL, RunFor::Cycles(3)).is_err(),
        "a graph with a custom node must reject a second run"
    );
}

/// Project the `u64` values out of a `Vec<(NanoTime, u64)>` accumulator.
fn evens_values(r: &wingfoil_next::interp::Runner, s: &Stream<Vec<(NanoTime, u64)>>) -> Vec<u64> {
    r.value(s).into_iter().map(|(_, v)| v).collect()
}
