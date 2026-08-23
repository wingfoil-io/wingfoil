//! Fixed-topology dynamic **routing**: `Builder::demux` and the two keyed forms
//! layered on it (`demux_map`, `demux_it`) — the wingfoil twins of legacy
//! `nodes/demux.rs`.
//!
//! Deliberately **not** gated on `dynamic-graph`, and split out of
//! `tests/dynamic_graph.rs` for that reason: demux adds and removes no nodes,
//! so it is available (and must stay tested) on default features, exactly as
//! legacy's ungated `nodes/demux.rs` is.

use std::time::Duration;

use wingfoil::interp::DemuxEvent;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// `demux` fixed-topology routing (twin of legacy `demux`, `nodes/demux.rs`):
/// the parent marks exactly the routed child each cycle — same-cycle, via the
/// engine's mark-dirty primitive — and each child re-emits the source's current
/// value only on the cycles it was selected, staying quiet otherwise. Routes a
/// counter (1..=6) by parity: even → child 0, odd → child 1.
#[test]
fn demux_routes_each_value_to_its_child() {
    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count().handle(); // 1, 2, …, 6
    let (children, _overflow) = g.with_builder(|b| b.demux(n, 2, |v: &u64| (*v % 2) as usize));
    let c0 = g.wrap(children[0]).accumulate();
    let c1 = g.wrap(children[1]).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    // Child 0 saw the even values, child 1 the odd ones — each only on its
    // selected cycles (accumulate collects only ticked values).
    assert_eq!(runner.value(&c0), vec![2u64, 4, 6], "child 0 (even)");
    assert_eq!(runner.value(&c1), vec![1u64, 3, 5], "child 1 (odd)");
}

/// `demux` overflow: a routed slot `>= size` lands on the overflow child.
/// Routing `value - 1` with `size = 2` sends 1 → child 0, 2 → child 1, and
/// 3, 4, 5, 6 → overflow.
#[test]
fn demux_routes_excess_to_overflow() {
    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count().handle();
    let (children, overflow) = g.with_builder(|b| b.demux(n, 2, |v: &u64| (*v - 1) as usize));
    let c0 = g.wrap(children[0]).accumulate();
    let c1 = g.wrap(children[1]).accumulate();
    let ov = g.wrap(overflow).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(runner.value(&c0), vec![1u64], "slot 0");
    assert_eq!(runner.value(&c1), vec![2u64], "slot 1");
    assert_eq!(
        runner.value(&ov),
        vec![3u64, 4, 5, 6],
        "overflow (slots >= size)"
    );
}

/// `demux_map` (twin of legacy `StreamOperators::demux`): the auto-assigning
/// `DemuxMap` key lifecycle over the raw `demux` primitive. Each new key claims
/// a free slot from the pool of `capacity`; `DemuxEvent::Close` routes to the
/// key's current slot then frees it; a key with no slot free lands on overflow.
///
/// With `capacity = 2`, feeding `(key, payload = cycle)`:
/// c1 (1,None)→slot0, c2 (2,None)→slot1, c3 (3,None)→overflow (pool full),
/// c4 (1,Close)→slot0 then frees it, c5 (4,None)→slot0 (reused), c6 (2,None)→slot1.
#[test]
fn demux_map_auto_assigns_and_releases_slots() {
    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1..=6
    // (key, event) schedule indexed by cycle; payload is the cycle number.
    let src = n
        .map(|c: &u64| {
            let (key, close) = match *c {
                1 => (1u64, false),
                2 => (2, false),
                3 => (3, false),
                4 => (1, true), // Close releases key 1's slot
                5 => (4, false),
                _ => (2, false),
            };
            (key, *c, close)
        })
        .handle();

    let (children, overflow) = g.with_builder(|b| {
        b.demux_map(src, 2, |(key, _payload, close): &(u64, u64, bool)| {
            let event = if *close {
                DemuxEvent::Close
            } else {
                DemuxEvent::None
            };
            (*key, event)
        })
    });
    // Collect the payload routed to each child.
    let c0 = g
        .wrap(children[0])
        .map(|(_, p, _): &(u64, u64, bool)| *p)
        .accumulate();
    let c1 = g
        .wrap(children[1])
        .map(|(_, p, _): &(u64, u64, bool)| *p)
        .accumulate();
    let ov = g
        .wrap(overflow)
        .map(|(_, p, _): &(u64, u64, bool)| *p)
        .accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(
        runner.value(&c0),
        vec![1u64, 4, 5],
        "slot 0: key1, key1 close, key4"
    );
    assert_eq!(runner.value(&c1), vec![2u64, 6], "slot 1: key2 twice");
    assert_eq!(runner.value(&ov), vec![3u64], "overflow: key3 (pool full)");
}

/// `demux_it` (twin of legacy `StreamOperators::demux_it`): routes *each item*
/// of an iterable source value to its keyed child, and every selected child
/// re-emits a `Burst` of exactly the items routed to it this cycle. Same
/// `DemuxMap` lifecycle as `demux_map`; unselected children stay quiet.
///
/// With `capacity = 2` and keys = the item value: cycle 1 emits `[10, 20]` (key
/// 10 → slot0, key 20 → slot1); cycle 2 emits `[10, 20, 30]` (30 is new with the
/// pool full → overflow).
#[test]
fn demux_it_routes_each_item_to_a_burst_per_child() {
    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2
    let src = n
        .map(|c: &u64| {
            if *c == 1 {
                vec![10u64, 20]
            } else {
                vec![10u64, 20, 30]
            }
        })
        .handle();

    let (children, overflow) =
        g.with_builder(|b| b.demux_it(src, 2, |v: &u64| (*v, DemuxEvent::None)));
    let c0 = g.wrap(children[0]).accumulate();
    let c1 = g.wrap(children[1]).accumulate();
    let ov = g.wrap(overflow).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(2)).unwrap();

    // child0 (key 10) and child1 (key 20) fire both cycles; overflow (key 30)
    // only cycle 2. Each burst holds exactly the items routed to that child.
    assert_eq!(
        runner.value(&c0),
        vec![burst![10u64], burst![10u64]],
        "child 0 (key 10)"
    );
    assert_eq!(
        runner.value(&c1),
        vec![burst![20u64], burst![20u64]],
        "child 1 (key 20)"
    );
    assert_eq!(runner.value(&ov), vec![burst![30u64]], "overflow (key 30)");
}

/// `Dispatch::FullSweep` is documented as an *executable reference oracle*
/// producing results identical to the default `Dispatch::Sparse` — it is what
/// differential parity tests compare against. That contract has to hold for
/// `demux` too, whose children are fired **only** by the same-cycle mark-dirty
/// buffer (they have no active upstream and no activation of their own).
#[test]
fn demux_routes_identically_under_the_full_sweep_oracle() {
    fn run(dispatch: wingfoil::interp::Dispatch) -> (Vec<u64>, Vec<u64>) {
        let g = GraphBuilder::new();
        let n = g.ticker(Duration::from_nanos(1)).count().handle();
        let (children, _overflow) = g.with_builder(|b| b.demux(n, 2, |v: &u64| (*v % 2) as usize));
        let c0 = g.wrap(children[0]).accumulate();
        let c1 = g.wrap(children[1]).accumulate();
        let mut runner = g.build().with_dispatch(dispatch);
        runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
        (runner.value(&c0), runner.value(&c1))
    }

    assert_eq!(
        run(wingfoil::interp::Dispatch::FullSweep),
        run(wingfoil::interp::Dispatch::Sparse),
        "FullSweep must reproduce Sparse exactly"
    );
}
