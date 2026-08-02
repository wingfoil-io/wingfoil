//! Runtime graph dynamism (feature `dynamic-graph`): appending nodes and
//! splicing edges onto a *live* interpreted graph mid-run. Each test is a next
//! twin of a classic wingfoil `dynamic-graph` oracle (`wingfoil/src/graph.rs`
//! `#[cfg(test)]`), reproducing its value/timing behaviour on the layered
//! `(layer, index)` engine.
#![cfg(feature = "dynamic-graph")]

use std::time::Duration;

use wingfoil_next::interp::Extension;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Twin of classic's `add_upstream_dynamically_fires_only_after_wired`
/// (`graph.rs:2102`): a node appended at the end of cycle 3 must first fire on
/// cycle 4, so across a 6-cycle run it fires exactly 3 times (cycles 4, 5, 6).
///
/// The appended node is a `fold` counting its own activations, so its final
/// value is the number of times it fired — the direct analogue of classic's
/// `extra_ticks` counter. It reads the shared per-cycle counter (`src`), which
/// ticks every cycle, so once wired it fires every subsequent cycle.
#[test]
fn append_node_fires_only_from_next_cycle() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(1)).count().handle();
    let mut runner = g.build();

    // Wire the counter at the end of cycle 3; capture its handle out of the hook.
    let mut appended: Option<_> = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |ext, cycle| {
            if cycle == 3 {
                appended = Some(ext.fold(src, 0u64, |acc, _| *acc += 1));
            }
            Ok(())
        })
        .unwrap();

    let appended = appended.expect("node appended at cycle 3");
    // Fired on cycles 4, 5, 6 only → 3 activations.
    assert_eq!(runner.value(appended), 3, "fires only after it was wired");
    // The pre-existing counter ran all 6 cycles.
    assert_eq!(runner.value(src), 6);
}

/// The appended node observes the *current* value of its source from the cycle
/// it goes live — not a stale or default value. Appended at end of cycle 2, a
/// `map` of the counter should read 3 on cycle 3, 4 on cycle 4, ….
#[test]
fn appended_node_reads_live_source_value() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(1)).count().handle();
    let mut runner = g.build();

    let mut mapped = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(5), |ext, cycle| {
            if cycle == 2 {
                mapped = Some(ext.map(src, |v: &u64| v * 10));
            }
            Ok(())
        })
        .unwrap();

    // Counter reaches 5; the map last fired on cycle 5 reading src=5 → 50.
    assert_eq!(runner.value(src), 5);
    assert_eq!(runner.value(mapped.unwrap()), 50);
}

/// Twin of classic's `layer_resort_after_deep_upstream_addition`
/// (`graph.rs:2371`): splicing a *deep* node in as an active upstream of a
/// *shallow* caller must lift the caller's layer above the deep node via
/// `fix_layers`, even though the caller has the lower node index.
#[test]
fn add_upstream_deep_resorts_caller_layer() {
    let g = GraphBuilder::new();
    let ticker = g.ticker(Duration::from_nanos(1));
    let depth1 = ticker.count(); // layer above the ticker
    let deep = depth1.map(|v: &u64| v * 2).map(|v: &u64| v + 1).handle(); // two layers deeper
    // A shallow caller triggered directly by the ticker.
    let caller = ticker.map(|_| 0u64).handle();
    let mut runner = g.build();

    let deep_layer_before = runner.layer_of(deep);
    let caller_layer_before = runner.layer_of(caller);
    assert!(
        caller_layer_before <= deep_layer_before,
        "precondition: caller starts at or below the deep node's layer \
         (caller={caller_layer_before}, deep={deep_layer_before})"
    );

    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(2), |ext, cycle| {
            if cycle == 1 {
                ext.add_upstream(caller, deep, true, false);
            }
            Ok(())
        })
        .unwrap();

    // After the splice the caller must sit strictly above the deep node.
    assert!(
        runner.layer_of(caller) > runner.layer_of(deep),
        "fix_layers must lift the caller above the deep upstream \
         (caller={}, deep={})",
        runner.layer_of(caller),
        runner.layer_of(deep),
    );
}

/// Twin of classic's `add_upstream_passive_does_not_trigger` (`graph.rs:2227`):
/// a node spliced in as a *passive* upstream is read but never triggers the
/// caller. Here the caller is a `fold` counting its activations; adding a
/// passive upstream must not increase that count — it keeps firing only on its
/// own active trigger.
#[test]
fn add_upstream_passive_does_not_trigger() {
    let g = GraphBuilder::new();
    // Two independent tickers at the same period so both fire every cycle.
    let trigger = g.ticker(Duration::from_nanos(1));
    let other = g.ticker(Duration::from_nanos(1)).count().handle();
    // Caller counts how often it fires; active upstream is `trigger` only.
    let caller = trigger.fold(0u64, |acc, _| *acc += 1).handle();
    let mut runner = g.build();

    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |ext, cycle| {
            if cycle == 2 {
                // Passive splice: caller reads `other` but is not triggered by it.
                ext.add_upstream(caller, other, false, false);
            }
            Ok(())
        })
        .unwrap();

    // Caller fired once per cycle on its own trigger — 6 times — regardless of
    // the passive edge added mid-run.
    assert_eq!(
        runner.value(caller),
        6,
        "a passive upstream must not add activations"
    );
}

/// Twin of classic's `remove_node_stops_firing_and_calls_lifecycle`
/// (`graph.rs:2064`): a node removed mid-run stops cycling immediately, and its
/// value freezes at whatever it last held.
///
/// A `fold` counter appended at cycle 1 fires on cycles 2, 3, 4 (→ 3), then is
/// removed at the cycle-4 boundary; over the remaining cycles it must not fire
/// again, so its final value stays 3 (not the 7 it would reach unremoved).
#[test]
fn removed_node_stops_firing_and_value_freezes() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(1)).count().handle();
    let mut runner = g.build();

    let mut counter = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(8), |ext, cycle| {
            if cycle == 1 {
                counter = Some(ext.fold(src, 0u64, |acc, _| *acc += 1));
            }
            if cycle == 4 {
                ext.remove(counter.expect("counter appended at cycle 1"))?;
            }
            Ok(())
        })
        .unwrap();

    let counter = counter.unwrap();
    // Fired on cycles 2, 3, 4 → 3; removed at the cycle-4 boundary, frozen after.
    // Its slot is tombstoned (not freed), so the value is still readable.
    assert_eq!(
        runner.value(counter),
        3,
        "removed node stops firing; value freezes"
    );
}

/// The lifecycle half of the removal oracle: a removed node runs its `stop`
/// then `teardown` exactly once, at removal — not again at run shutdown. Uses a
/// `finally` node (whose whole purpose is an observable teardown hook) and
/// checks the count both immediately after removal and after the run ends.
#[test]
fn remove_runs_teardown_once_at_removal() {
    use std::cell::Cell;
    use std::rc::Rc;

    let teardowns = Rc::new(Cell::new(0u64));
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(1)).count().handle();
    let tc = teardowns.clone();
    // `finally`'s closure runs at teardown; count how many times.
    let fin = g.with_builder(|b| {
        b.finally(src, move |_| {
            tc.set(tc.get() + 1);
            Ok(())
        })
    });
    let mut runner = g.build();

    let mut teardowns_at_removal = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |ext, cycle| {
            if cycle == 3 {
                ext.remove(fin)?;
                teardowns_at_removal = Some(teardowns.get());
            }
            Ok(())
        })
        .unwrap();

    // Teardown ran once, *at* removal (observed mid-run) …
    assert_eq!(
        teardowns_at_removal,
        Some(1),
        "teardown runs when the node is removed"
    );
    // … and was not called a second time by the end-of-run cleanup.
    assert_eq!(
        teardowns.get(),
        1,
        "teardown is not called again at shutdown"
    );
}

/// Twin of classic's `add_upstream_with_recycle_delivers_first_value`
/// (`graph.rs:2164`): with `recycle = true`, a node appended over a *quiet*
/// source is scheduled to fire at `time + 1`, so it observes the source's real
/// current value rather than the `Default` it would otherwise hold.
///
/// The source is a `constant`, which ticks once at t=0 then stays quiet. A `map`
/// of it spliced in at cycle 3 would never fire on its own (the constant never
/// ticks again); recycle forces one evaluation, delivering `42 + 1 = 43`.
#[test]
fn recycle_delivers_first_value_from_quiet_source() {
    let g = GraphBuilder::new();
    let c = g.constant(42u64).handle(); // ticks once at t=0, then quiet
    let trigger = g.ticker(Duration::from_nanos(1));
    let caller = trigger.fold(0u64, |acc, _| *acc += 1).handle();
    let mut runner = g.build();

    let mut mapped = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |ext, cycle| {
            if cycle == 3 {
                let m = ext.map(c, |v: &u64| v + 1);
                ext.add_upstream(caller, m, true, true); // recycle = true
                mapped = Some(m);
            }
            Ok(())
        })
        .unwrap();

    assert_eq!(
        runner.value(mapped.unwrap()),
        43,
        "recycle scheduled the appended node to observe the constant's value"
    );
}

/// The negative control for recycle: without it, a node appended over the same
/// *quiet* source never fires and keeps its `Default` — proving the previous
/// test's `43` is the recycle schedule at work, not natural propagation.
#[test]
fn without_recycle_quiet_source_stays_default() {
    let g = GraphBuilder::new();
    let c = g.constant(42u64).handle();
    let trigger = g.ticker(Duration::from_nanos(1));
    let caller = trigger.fold(0u64, |acc, _| *acc += 1).handle();
    let mut runner = g.build();

    let mut mapped = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |ext, cycle| {
            if cycle == 3 {
                let m = ext.map(c, |v: &u64| v + 1);
                ext.add_upstream(caller, m, true, false); // recycle = false
                mapped = Some(m);
            }
            Ok(())
        })
        .unwrap();

    assert_eq!(
        runner.value(mapped.unwrap()),
        0,
        "without recycle the quiet source never re-ticks the appended node"
    );
}

/// End-to-end twin of the `dynamic-group` example (`examples/dynamic`): a keyed
/// price book maintained by an in-graph `dynamic_group` node that wires per-key
/// filter sub-graphs on `add`, tears them down on `del`, and folds each live
/// member's current price into a `BTreeMap` — all driven from *inside* the graph
/// (no driver-hook mutation). Exercises the whole stack: in-cycle staging,
/// active/recycle splicing, removal, and the fresh-read guarantee (each member
/// is an active upstream, so the group reads its current value).
///
/// A shared feed emits `(key, price)` alternating key 1/0 each cycle with
/// `price = cycle * 10`. Key 0 is added at cycle 1, key 1 at cycle 2, and key 0
/// is deleted at cycle 4.
#[test]
fn dynamic_group_maintains_a_live_price_book() {
    use std::collections::BTreeMap;

    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2, 3, …
    // Shared feed: (key, price) with key alternating 1,0,1,0,… and price = 10*cycle.
    let feed = n.map(|c: &u64| (c % 2, c * 10)).handle();
    // `add` fires key 0 at cycle 1 and key 1 at cycle 2. `del` fires key 0 at
    // cycle 4, and a no-op delete of a non-existent key at cycle 5 — the latter
    // exists to trigger the group *early* (via its `del` edge) on a cycle where
    // key 1's member also fires. Without `fix_layers` re-sorting the group above
    // its members, that early trigger drains the group before key 1's member,
    // so it would miss the cycle-5 price (50) and the book would hold a stale 30.
    let add = n
        .map_filter(|c: &u64| ((if *c == 1 { 0u64 } else { 1 }), *c == 1 || *c == 2))
        .handle();
    let del = n
        .map_filter(|c: &u64| ((if *c == 4 { 0u64 } else { 99 }), *c == 4 || *c == 5))
        .handle();

    let book = g.with_builder(|b| {
        b.dynamic_group(
            add,
            del,
            // Factory: per-key subgraph = filter the feed to this key, take price.
            move |ext: &mut Extension<'_>, k: u64| {
                let selected = ext.filter_value(feed, move |(i, _): &(u64, u64)| *i == k);
                ext.map(selected, |(_, px): &(u64, u64)| *px)
            },
            BTreeMap::<u64, u64>::new(),
            |book: &mut BTreeMap<u64, u64>, key: &u64, price: &u64| {
                book.insert(*key, *price);
            },
            |book: &mut BTreeMap<u64, u64>, key: &u64| {
                book.remove(key);
            },
        )
    });

    let mut runner = g.build();
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |_ext, _cycle| Ok(()))
        .unwrap();

    // key0 tracked prices on cycles 2 (20), removed at cycle 4; key1 tracked on
    // cycles 3 (30) and 5 (50). Final book holds only the live key 1 at 50.
    let final_book = runner.value(book);
    assert_eq!(
        final_book,
        BTreeMap::from([(1u64, 50u64)]),
        "final price book"
    );
}

/// `demux` fixed-topology routing (twin of classic `demux`, `nodes/demux.rs`):
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

/// A key type that is `Hash + Eq` but deliberately **not** `Ord` — so it cannot
/// back a `BTreeMap`. `dynamic_group_with_store(HashMap::new(), …)` must accept
/// it (and would fail to compile if a `K: Ord` bound leaked through).
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
struct SymbolId(u64);

/// `StreamStore` parity: the same live-price-book group as
/// `dynamic_group_maintains_a_live_price_book`, but backed by a `HashMap` keyed
/// by a non-`Ord` `SymbolId`. Proves the pluggable backing store lets a
/// `Hash + Eq` key flow through where the default `BTreeMap` (`K: Ord`) could
/// not. The final assertion compares whole `HashMap`s, so `HashMap`'s
/// nondeterministic iteration order does not affect it.
#[test]
fn dynamic_group_with_store_supports_non_ord_hashmap_key() {
    use std::collections::HashMap;

    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2, 3, …
    // Shared feed: (SymbolId(key), price) with key alternating 1,0,1,0,… and
    // price = 10*cycle.
    let feed = n.map(|c: &u64| (SymbolId(c % 2), c * 10)).handle();
    // Same schedule as the BTreeMap oracle: add key0 at cycle 1, key1 at cycle
    // 2; delete key0 at cycle 4, with a no-op delete of a missing key at cycle 5
    // to trigger the group early (the fix_layers exercise).
    let add = n
        .map_filter(|c: &u64| (SymbolId(if *c == 1 { 0 } else { 1 }), *c == 1 || *c == 2))
        .handle();
    let del = n
        .map_filter(|c: &u64| (SymbolId(if *c == 4 { 0 } else { 99 }), *c == 4 || *c == 5))
        .handle();

    let book = g.with_builder(|b| {
        b.dynamic_group_with_store(
            add,
            del,
            move |ext: &mut Extension<'_>, k: SymbolId| {
                let selected = ext.filter_value(feed, move |(i, _): &(SymbolId, u64)| *i == k);
                ext.map(selected, |(_, px): &(SymbolId, u64)| *px)
            },
            // Members store: a HashMap keyed by the non-Ord SymbolId. Its value
            // type (the engine's internal LiveStream) is inferred, so no private
            // type is named here.
            HashMap::new(),
            // Output book (the aggregated value V).
            HashMap::<SymbolId, u64>::new(),
            |book: &mut HashMap<SymbolId, u64>, key: &SymbolId, price: &u64| {
                book.insert(key.clone(), *price);
            },
            |book: &mut HashMap<SymbolId, u64>, key: &SymbolId| {
                book.remove(key);
            },
        )
    });

    let mut runner = g.build();
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |_ext, _cycle| Ok(()))
        .unwrap();

    // key0 tracked on cycle 2 (20), removed at cycle 4; key1 tracked on cycles 3
    // (30) and 5 (50). Final book holds only the live key 1 at 50.
    let final_book = runner.value(book);
    assert_eq!(
        final_book,
        HashMap::from([(SymbolId(1), 50u64)]),
        "final price book (HashMap-backed, non-Ord key)"
    );
}

/// `demux_map` (twin of classic `StreamOperators::demux`): the auto-assigning
/// `DemuxMap` key lifecycle over the raw `demux` primitive. Each new key claims
/// a free slot from the pool of `capacity`; `DemuxEvent::Close` routes to the
/// key's current slot then frees it; a key with no slot free lands on overflow.
///
/// With `capacity = 2`, feeding `(key, payload = cycle)`:
/// c1 (1,None)→slot0, c2 (2,None)→slot1, c3 (3,None)→overflow (pool full),
/// c4 (1,Close)→slot0 then frees it, c5 (4,None)→slot0 (reused), c6 (2,None)→slot1.
#[test]
fn demux_map_auto_assigns_and_releases_slots() {
    use wingfoil_next::interp::DemuxEvent;

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

/// `demux_it` (twin of classic `StreamOperators::demux_it`): routes *each item*
/// of an iterable source value to its keyed child, and every selected child
/// re-emits a `Burst` of exactly the items routed to it this cycle. Same
/// `DemuxMap` lifecycle as `demux_map`; unselected children stay quiet.
///
/// With `capacity = 2` and keys = the item value: cycle 1 emits `[10, 20]` (key
/// 10 → slot0, key 20 → slot1); cycle 2 emits `[10, 20, 30]` (30 is new with the
/// pool full → overflow).
#[test]
fn demux_it_routes_each_item_to_a_burst_per_child() {
    use wingfoil_next::interp::DemuxEvent;

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
