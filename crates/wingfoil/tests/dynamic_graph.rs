//! Runtime graph dynamism (feature `dynamic-graph`): appending nodes and
//! splicing edges onto a *live* interpreted graph mid-run. Each test is a wingfoil
//! twin of a legacy wingfoil `dynamic-graph` oracle (`legacy/wingfoil/src/graph.rs`
//! `#[cfg(test)]`), reproducing its value/timing behaviour on the layered
//! `(layer, index)` engine.
#![cfg(feature = "dynamic-graph")]

use std::time::Duration;

use wingfoil::interp::Extension;
// `Builder::finally` is `#[op]`-generated, and since #782 those methods arrive
// on per-op extension traits (`__WfBuildFinally`) so the attribute can expand
// out-of-crate as well as in.
use wingfoil::ops::*;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Twin of legacy's `add_upstream_dynamically_fires_only_after_wired`
/// (`graph.rs:2102`): a node appended at the end of cycle 3 must first fire on
/// cycle 4, so across a 6-cycle run it fires exactly 3 times (cycles 4, 5, 6).
///
/// The appended node is a `fold` counting its own activations, so its final
/// value is the number of times it fired — the direct analogue of legacy's
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

/// Twin of legacy's `layer_resort_after_deep_upstream_addition`
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

/// Twin of legacy's `add_upstream_passive_does_not_trigger` (`graph.rs:2227`):
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

/// Twin of legacy's `remove_node_stops_firing_and_calls_lifecycle`
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

/// Twin of legacy's `add_upstream_with_recycle_delivers_first_value`
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

/// End-to-end twin of the `dynamic-group` example
/// (`examples/core/dynamism/dynamic_group`): a keyed
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

/// `Extension::combine` covers what the five original methods could not: a
/// member built from *several* typed upstreams rather than filtered from one.
/// Same schedule as `dynamic_group_maintains_a_live_price_book` — key 0 added at
/// cycle 1 and deleted at cycle 4, key 1 added at cycle 2 — but now each key's
/// own price is combined with a shared `forward` level that is not per-key
/// state and ticks every cycle.
///
/// The per-cycle books pin the gather as well as the fan-in: key 1's first
/// cycle (t=2) reads both its own price and the forward, and the next one (t=3)
/// carries the forward alone because its key did not quote that instant.
#[test]
fn dynamic_group_member_combines_a_per_key_stream_with_a_shared_one() {
    use std::collections::BTreeMap;

    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2, 3, …
    // Per-key feed: (key, price) with key alternating 1,0,… and price = 10*cycle.
    let feed = n.map(|c: &u64| (c % 2, c * 10)).handle();
    // A shared level that is explicitly not per-key state: forward = cycle.
    let forward = n.map(|c: &u64| *c).handle();
    // Same add/del schedule as the price-book oracle above.
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
            move |ext: &mut Extension<'_>, k: u64| {
                // Each member selects its key's price and fans it in with the
                // shared forward. Both are `u64`; the burst carries whichever
                // ticked this cycle.
                let mine = ext.filter_value(feed, move |(i, _): &(u64, u64)| *i == k);
                let price = ext.map(mine, |(_, px): &(u64, u64)| *px);
                let joined = ext.combine(&[price, forward]);
                ext.map(joined, |b: &Burst<u64>| b.iter().sum::<u64>())
            },
            BTreeMap::<u64, u64>::new(),
            |book: &mut BTreeMap<u64, u64>, key: &u64, v: &u64| {
                book.insert(*key, *v);
            },
            |book: &mut BTreeMap<u64, u64>, key: &u64| {
                book.remove(key);
            },
        )
    });
    let history = g.wrap(book).with_time().accumulate();

    let mut runner = g.build();
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(6), |_ext, _cycle| Ok(()))
        .unwrap();

    // key0 is live from cycle 2 and folds 20+2 before it is deleted at cycle 4
    // (the delete drops it from the store before that cycle's fold). key1 is
    // live from cycle 3 and adds the forward alone on cycles where its own key
    // did not quote.
    assert_eq!(
        runner.value(history),
        vec![
            (NanoTime::new(1), BTreeMap::from([(0u64, 22u64)])),
            (
                NanoTime::new(2),
                BTreeMap::from([(0u64, 3u64), (1u64, 33u64)])
            ),
            (NanoTime::new(3), BTreeMap::from([(1u64, 4u64)])),
            (NanoTime::new(4), BTreeMap::from([(1u64, 55u64)])),
            (NanoTime::new(5), BTreeMap::from([(1u64, 6u64)])),
        ],
        "per-cycle joined price book"
    );
}

/// The fan-in boundary: `Extension::combine` accepts a one-element slice, the
/// same shape `Builder::combine` does. The combined handle is read directly, so
/// this pins the burst contents rather than a downstream reduction.
#[test]
fn extension_combine_of_one_source_gathers_a_single_value() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(1)).count().handle();
    let mut runner = g.build();

    let mut combined = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(4), |ext, cycle| {
            if cycle == 1 {
                combined = Some(ext.combine(&[src]));
            }
            Ok(())
        })
        .unwrap();

    // Appended at the end of cycle 1, so it fires on cycles 2..4; at cycle 4
    // `src` holds 4 and the burst carries exactly that one value.
    assert_eq!(
        runner.value(combined.unwrap()),
        Burst::from([4u64]),
        "a one-source combine is a burst of one"
    );
}

/// A `combine` node reads every source's current value on its first cycle and
/// gathers only the sources that ticked on later ones. The fast and slow ticker
/// periods are the same as `tests/combine_n.rs`'s mask oracle, so the sequence
/// visits a shared instant and fast-only instants.
#[test]
fn extension_combine_reads_every_source_first_then_masks_ticks() {
    let g = GraphBuilder::new();
    let fast = g.ticker(Duration::from_nanos(1)).count().handle();
    let slow = g.ticker(Duration::from_nanos(3)).count().handle();
    let mut runner = g.build();

    let mut gathered = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(5), |ext, cycle| {
            if cycle == 1 {
                let joined = ext.combine(&[fast, slow]);
                gathered = Some(ext.fold(
                    joined,
                    Vec::new(),
                    |acc: &mut Vec<Vec<u64>>, b: &Burst<u64>| acc.push(b.iter().copied().collect()),
                ));
            }
            Ok(())
        })
        .unwrap();

    // Appended at the end of cycle 1. Its first cycle (t=1) carries both
    // current values — fast's fresh 2 and slow's held 1 — then t=2 carries the
    // fast alone, t=3 both again, and t=4 the fast alone.
    assert_eq!(
        runner.value(gathered.unwrap()),
        vec![vec![2u64, 1u64], vec![3], vec![4, 2], vec![5]],
        "the first cycle reads every source, then the tick mask holds"
    );
}

/// The recycle guarantee has to hold for a `combine` member too. `dynamic_group`
/// splices every member in with `recycle`, so its first cycle runs whether or
/// not an upstream ticks in that instant; `map` reads its source's held value
/// there, and a `combine` member must start from the same place rather than
/// stay quiet until an upstream's next tick.
///
/// A shared ticker with period 4 feeds the member, which is added at cycle 2.
/// The member's recycle cycle is t=2, between the ticker's t=0 and t=4 firings,
/// so the key has to first appear at t=2 carrying the ticker's held value 1 —
/// not at t=4.
#[test]
fn dynamic_group_combine_member_fires_on_the_recycle_cycle() {
    use std::collections::BTreeMap;

    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2, 3, …
    // Shared slow ticker: fires at t=0, 4, 8, … with 1, 2, 3, …
    let slow = g.ticker(Duration::from_nanos(4)).count().handle();
    // Add key 7 at cycle 2; nothing is ever deleted.
    let add = n.map_filter(|c: &u64| (7u64, *c == 2)).handle();
    let del = n.map_filter(|_: &u64| (7u64, false)).handle();

    let book = g.with_builder(|b| {
        b.dynamic_group(
            add,
            del,
            move |ext: &mut Extension<'_>, _k: u64| {
                let joined = ext.combine(&[slow]);
                ext.map(joined, |b: &Burst<u64>| b.iter().sum::<u64>())
            },
            BTreeMap::<u64, u64>::new(),
            |book: &mut BTreeMap<u64, u64>, key: &u64, v: &u64| {
                book.insert(*key, *v);
            },
            |book: &mut BTreeMap<u64, u64>, key: &u64| {
                book.remove(key);
            },
        )
    });
    let history = g.wrap(book).with_time().accumulate();

    let mut runner = g.build();
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(10), |_ext, _cycle| Ok(()))
        .unwrap();

    assert_eq!(
        runner.value(history),
        vec![
            (NanoTime::new(2), BTreeMap::from([(7u64, 1u64)])),
            (NanoTime::new(4), BTreeMap::from([(7u64, 2u64)])),
            (NanoTime::new(8), BTreeMap::from([(7u64, 3u64)])),
        ],
        "the member appears on its recycle cycle, not only when the ticker next fires"
    );
}

/// A source appended in the same `Extension` scope must not contribute its seed
/// slot on the first cycle. Here the price-book factory's `price` is a fresh
/// `filter_value` + `map`, and key 0 is added on a cycle where the feed quotes
/// key 1, so `price` has never ticked and still holds `A::default()`. The burst
/// must carry the forward alone, not `[0, 3]`.
#[test]
fn dynamic_group_combine_member_drops_a_new_source_that_never_ticked() {
    use std::collections::BTreeMap;

    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2, 3, …
    // feed: key = n % 2, so n=2 quotes key 0 and n=3 quotes key 1.
    let feed = n.map(|c: &u64| (c % 2, c * 10)).handle();
    let forward = n.map(|c: &u64| *c).handle();
    // Add key 0 at cycle 2, so its member's first cycle is t=2 — a key-1 quote.
    let add = n.map_filter(|c: &u64| (0u64, *c == 2)).handle();
    let del = n.map_filter(|_: &u64| (0u64, false)).handle();

    let book = g.with_builder(|b| {
        b.dynamic_group(
            add,
            del,
            move |ext: &mut Extension<'_>, k: u64| {
                let mine = ext.filter_value(feed, move |(i, _): &(u64, u64)| *i == k);
                let price = ext.map(mine, |(_, px): &(u64, u64)| *px);
                let joined = ext.combine(&[price, forward]);
                ext.map(joined, |b: &Burst<u64>| {
                    b.iter().copied().collect::<Vec<u64>>()
                })
            },
            BTreeMap::<u64, Vec<u64>>::new(),
            |book: &mut BTreeMap<u64, Vec<u64>>, key: &u64, b: &Vec<u64>| {
                book.insert(*key, b.clone());
            },
            |book: &mut BTreeMap<u64, Vec<u64>>, key: &u64| {
                book.remove(key);
            },
        )
    });
    let history = g.wrap(book).with_time().accumulate();

    let mut runner = g.build();
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(5), |_ext, _cycle| Ok(()))
        .unwrap();

    // t=2 is the member's first cycle and key 1's quote is the only one there,
    // so the burst carries the forward alone; t=3 is a key-0 quote, so both ride.
    assert_eq!(
        runner.value(history),
        vec![
            (NanoTime::new(2), BTreeMap::from([(0u64, vec![3u64])])),
            (
                NanoTime::new(3),
                BTreeMap::from([(0u64, vec![40u64, 4u64])])
            ),
            (NanoTime::new(4), BTreeMap::from([(0u64, vec![5u64])])),
        ],
        "a source that has never ticked contributes nothing"
    );
}

/// A `combine` whose sources were all appended in the same scope has no
/// pre-existing upstream, so `recycle_schedule` never schedules it: its first
/// run is an ordinary upstream tick. The first-cycle rule must not read the
/// other, still-quiet source's seed slot there either.
#[test]
fn extension_combine_drops_a_new_source_that_has_not_ticked() {
    let g = GraphBuilder::new();
    let n = g.ticker(Duration::from_nanos(1)).count(); // 1, 2, 3, …
    let mut runner = g.build();

    let mut history = None;
    runner
        .run_dynamic(HISTORICAL, RunFor::Cycles(5), |ext, cycle| {
            if cycle == 1 {
                let even = ext.filter_value(&n, |c: &u64| c.is_multiple_of(2));
                let odd = ext.filter_value(&n, |c: &u64| !c.is_multiple_of(2));
                let joined = ext.combine(&[even, odd]);
                history = Some(ext.fold(
                    joined,
                    Vec::new(),
                    |acc: &mut Vec<Vec<u64>>, b: &Burst<u64>| acc.push(b.iter().copied().collect()),
                ));
            }
            Ok(())
        })
        .unwrap();

    // Added at cycle 1, so it is live from t=1: n=2 ticks the even side and the
    // odd side has never run, then alternates. The seed 0 must never appear.
    assert_eq!(
        runner.value(history.unwrap()),
        vec![vec![2u64], vec![3], vec![4], vec![5]],
        "the never-ticked source contributes nothing on the first run either"
    );
}
