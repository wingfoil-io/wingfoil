//! Runtime graph dynamism (feature `dynamic-graph`): appending nodes and
//! splicing edges onto a *live* interpreted graph mid-run. Each test is a next
//! twin of a classic wingfoil `dynamic-graph` oracle (`wingfoil/src/graph.rs`
//! `#[cfg(test)]`), reproducing its value/timing behaviour on the layered
//! `(layer, index)` engine.
#![cfg(feature = "dynamic-graph")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

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
