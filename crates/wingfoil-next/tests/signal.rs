//! The builder-less [`Signal`] facade: whole programs in the legacy idiom —
//! free source functions, `stream.run(...)`, `stream.peek_value()` — with
//! every stream backed by the new `Op`/`Builder` engine.
//!
//! `Signal`'s combinators are generated from the op catalog by
//! `#[op(build = x, fluent)]`, so this file is where the generated surface is
//! *exercised*: that each method's signature is the one a caller expects, and
//! that it computes what its fluent twin does. `op_fluent_shapes.rs` pins the
//! generator's shapes; these are the semantics.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wingfoil_next::signal::{constant, ticker};
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// The canonical legacy snippet: count a ticker and read the result.
#[test]
fn legacy_counter_runs_on_the_new_engine() {
    let counted = ticker(Duration::from_nanos(100)).count();
    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());
}

/// A legacy chain: ticker → count → map → filter → accumulate, run and read
/// off the accumulator, all in the legacy idiom.
#[test]
fn legacy_chain_maps_filters_accumulates() {
    let count = ticker(Duration::from_nanos(100)).count();
    let is_even = count.map(|i| i.is_multiple_of(2));
    let evens = count.filter(&is_even).accumulate();
    evens.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![2, 4, 6], evens.peek_value());
}

/// A legacy fold (running sum) driven off a counter.
#[test]
fn legacy_fold_sums() {
    let total = ticker(Duration::from_nanos(100))
        .count()
        .fold(0u64, |acc, v| *acc += v);
    total.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // 1 + 2 + 3 + 4
    assert_eq!(10, total.peek_value());
}

/// Legacy `constant` + `delay`, matching the legacy engine's timing.
#[test]
fn legacy_constant_and_delay() {
    let delayed = constant(7u64).delay(Duration::from_nanos(50)).accumulate();
    delayed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // constant ticks once at t=0; delayed re-emits it at t=50.
    assert_eq!(vec![7], delayed.peek_value());
}

/// Peeking before the graph has run is a reachable user error. `peek_value`
/// mirrors the legacy infallible signature (`-> T`), so it enforces the
/// precondition with an explanatory panic rather than an out-of-bounds one.
#[test]
#[should_panic(expected = "Signal::run must be called before Signal::peek_value")]
fn peek_before_run_panics_with_a_clear_message() {
    let counted = ticker(Duration::from_nanos(100)).count();
    // No `run` — this must panic with the documented precondition message,
    // never a bare index-out-of-bounds.
    let _ = counted.peek_value();
}

/// Re-running is supported (spike 0.4's setup-per-run reset): the graph is
/// built once and the runner retained, and each `run` restores every node's
/// state and value slot first, so two runs of the *same* `Signal` produce
/// identical results — never the accumulator-continues bug (a count that would
/// go 5 → 10). This is the wingfoil-python re-run gate.
#[test]
fn second_run_matches_the_first() {
    let counted = ticker(Duration::from_nanos(100)).count();

    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());

    // A second run reproduces the first exactly — the count restarts from the
    // fold's wiring-time seed rather than continuing at 5.
    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());

    // A different bound on the retained runner still gives fresh-graph results.
    counted.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(3, counted.peek_value());
}

// --- Expanded operator surface, all in the legacy idiom -------------------

/// `limit` passes the first N values, then stays quiet.
#[test]
fn legacy_limit_passes_first_n() {
    let count = ticker(Duration::from_nanos(100)).count();
    let limited = count.limit(3).accumulate();
    limited.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 2, 3], limited.peek_value());
}

/// `distinct` suppresses consecutive duplicates (emit on change only).
#[test]
fn legacy_distinct_drops_repeats() {
    // counts 1..=6 mapped through integer halving: 0,1,1,2,2,3 -> distinct 0,1,2,3
    let count = ticker(Duration::from_nanos(100)).count();
    let stepped = count.map(|i| i / 2).distinct().accumulate();
    stepped.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![0, 1, 2, 3], stepped.peek_value());
}

/// `difference` emits `value - previous`, quiet on the first value.
#[test]
fn legacy_difference_of_successive_values() {
    // squares 1,4,9,16 -> successive differences 3,5,7
    let count = ticker(Duration::from_nanos(100)).count();
    let diffs = count.map(|i| i * i).difference().accumulate();
    diffs.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![3, 5, 7], diffs.peek_value());
}

/// `not` negates each value — here a parity flag.
#[test]
fn legacy_not_negates() {
    let count = ticker(Duration::from_nanos(100)).count();
    // is_even over counts 1,2,3,4 -> false,true,false,true; not -> true,false,true,false
    let flipped = count.map(|i| i.is_multiple_of(2)).not().accumulate();
    flipped.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![true, false, true, false], flipped.peek_value());
}

/// `merge` recombines two disjoint sub-streams of one source; exactly one
/// ticks each cycle, so the merge reconstructs the original sequence.
#[test]
fn legacy_merge_recombines_disjoint_streams() {
    let count = ticker(Duration::from_nanos(100)).count();
    let evens = count.filter(&count.map(|i| i.is_multiple_of(2)));
    let odds = count.filter(&count.map(|i| !i.is_multiple_of(2)));
    let merged = odds.merge(&evens).accumulate();
    merged.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![1, 2, 3, 4], merged.peek_value());
}

/// `sample` reads the current value of one stream whenever a trigger ticks.
#[test]
fn legacy_sample_reads_on_trigger() {
    let tk = ticker(Duration::from_nanos(100));
    let count = tk.count();
    let value = count.map(|i| i * 10); // 10,20,30,40,50,60
    // trigger ticks only on even counts (cycles 2,4,6)
    let trigger = tk.filter(&count.map(|i| i.is_multiple_of(2)));
    let sampled = value.sample(&trigger).accumulate();
    sampled.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![20, 40, 60], sampled.peek_value());
}

/// `with_time` pairs each value with the engine time it ticked at.
#[test]
fn legacy_with_time_pairs_time_and_value() {
    let timed = ticker(Duration::from_nanos(100))
        .count()
        .with_time()
        .accumulate();
    timed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::from(0u64), 1u64),
            (NanoTime::from(100u64), 2u64),
            (NanoTime::from(200u64), 3u64),
        ],
        timed.peek_value()
    );
}

/// `ticked_at` emits the engine time on each tick.
#[test]
fn legacy_ticked_at_emits_engine_time() {
    let times = ticker(Duration::from_nanos(100)).ticked_at().accumulate();
    times.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            NanoTime::from(0u64),
            NanoTime::from(100u64),
            NanoTime::from(200u64),
        ],
        times.peek_value()
    );
}

/// `ticked_at_elapsed` emits time since the run start (start = 0 here).
#[test]
fn legacy_ticked_at_elapsed_emits_elapsed() {
    let elapsed = ticker(Duration::from_nanos(100))
        .ticked_at_elapsed()
        .accumulate();
    elapsed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(
        vec![
            NanoTime::from(0u64),
            NanoTime::from(100u64),
            NanoTime::from(200u64),
        ],
        elapsed.peek_value()
    );
}

/// `throttle` rate-limits emission to at most once per interval.
#[test]
fn legacy_throttle_rate_limits() {
    // ticks at t=0,100,200,300,400; throttle(250) admits t=0 then t=300
    let throttled = ticker(Duration::from_nanos(100))
        .count()
        .throttle(Duration::from_nanos(250))
        .accumulate();
    throttled.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 4], throttled.peek_value());
}

/// `window` buffers values and flushes them on each interval boundary.
#[test]
fn legacy_window_flushes_on_interval() {
    let windowed = ticker(Duration::from_nanos(100))
        .count()
        .window(Duration::from_nanos(300))
        .accumulate();
    windowed.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![vec![1, 2, 3], vec![4, 5, 6]], windowed.peek_value());
}

/// `buffer` flushes a `Vec` once `capacity` values accumulate (plus a final
/// partial flush on the last cycle).
#[test]
fn legacy_buffer_flushes_by_capacity() {
    let buffered = ticker(Duration::from_nanos(100))
        .count()
        .buffer(2)
        .accumulate();
    buffered.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![vec![1, 2], vec![3, 4], vec![5]], buffered.peek_value());
}

// --- Newly surfaced operator methods, all in the legacy idiom -------------

/// `try_map` applies a fallible closure; the `Ok` path passes values through.
#[test]
fn legacy_try_map_transforms_values() {
    let doubled = ticker(Duration::from_nanos(100))
        .count()
        .try_map(|i: &u64| Ok(*i * 2))
        .accumulate();
    doubled.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![2, 4, 6], doubled.peek_value());
}

/// `map_filter` maps and drops in one pass, preserving values **and** the tick
/// times of the values it keeps.
#[test]
fn legacy_map_filter_keeps_even_counts_with_times() {
    // counts 1,2,3,4 at t=0,100,200,300; keep even counts → (2@100, 4@300)
    let evens = ticker(Duration::from_nanos(100))
        .count()
        .map_filter(|i: &u64| (*i, i.is_multiple_of(2)))
        .with_time()
        .accumulate();
    evens.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(
        vec![
            (NanoTime::from(100u64), 2u64),
            (NanoTime::from(300u64), 4u64),
        ],
        evens.peek_value()
    );
}

/// `filter_map` keeps `Some`, drops `None`.
#[test]
fn legacy_filter_map_keeps_some() {
    // counts 1..=6; keep squares of odd inputs → 1, 9, 25
    let out = ticker(Duration::from_nanos(100))
        .count()
        .filter_map(|i: &u64| (i % 2 == 1).then_some(i * i))
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![1, 9, 25], out.peek_value());
}

/// `filter_value` drops values failing a predicate.
#[test]
fn legacy_filter_value_drops_on_predicate() {
    // counts 1..=5; keep > 3 → 4, 5
    let out = ticker(Duration::from_nanos(100))
        .count()
        .filter_value(|i: &u64| *i > 3)
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![4, 5], out.peek_value());
}

/// `reduce` folds from `T::default()`, applying `f(acc, value)` — a running sum.
#[test]
fn legacy_reduce_running_sum() {
    // counts 1,2,3,4; running sum 1,3,6,10
    let out = ticker(Duration::from_nanos(100))
        .count()
        .reduce(|acc: &u64, v: &u64| acc + v)
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![1, 3, 6, 10], out.peek_value());
}

/// `produce` emits a fresh value on each tick, ignoring the source value.
#[test]
fn legacy_produce_emits_on_each_tick() {
    let out = ticker(Duration::from_nanos(100))
        .produce(|| 42u64)
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![42, 42, 42], out.peek_value());
}

/// `inspect` runs a side effect while passing values through unchanged.
#[test]
fn legacy_inspect_taps_and_passes_through() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    let out = ticker(Duration::from_nanos(100))
        .count()
        .inspect(move |v: &u64| s.borrow_mut().push(*v))
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], out.peek_value());
    assert_eq!(vec![1, 2, 3], *seen.borrow());
}

/// `for_each` runs a side-effecting sink on each tick.
#[test]
fn legacy_for_each_sinks_each_value() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    let sink = ticker(Duration::from_nanos(100))
        .count()
        .for_each(move |v: &u64| {
            s.borrow_mut().push(*v);
            Ok(())
        });
    sink.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], *seen.borrow());
}

/// `finally` runs once at teardown, observing the last value.
#[test]
fn legacy_finally_runs_at_teardown() {
    let last = Rc::new(RefCell::new(0u64));
    let f = last.clone();
    let sink = ticker(Duration::from_nanos(100))
        .count()
        .finally(move |v: &u64| {
            *f.borrow_mut() = *v;
            Ok(())
        });
    sink.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(3, *last.borrow());
}

/// `collapse` reduces an iterable value to its last item, quiet when empty.
#[test]
fn legacy_collapse_takes_last_item() {
    // counts 1,2,3 → vecs [1,10],[2,20],[3,30] → collapse → 10,20,30
    let out = ticker(Duration::from_nanos(100))
        .count()
        .map(|i: &u64| vec![*i, *i * 10])
        .collapse::<u64>()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![10, 20, 30], out.peek_value());
}

/// `timed` passes values through unchanged (it only prints a summary).
#[test]
fn legacy_timed_passes_through() {
    let out = ticker(Duration::from_nanos(100))
        .count()
        .timed()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], out.peek_value());
}

/// `print` passes values through unchanged (it only prints at teardown).
#[test]
fn legacy_print_passes_through() {
    let out = ticker(Duration::from_nanos(100))
        .count()
        .print()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], out.peek_value());
}

/// `delay_with_reset` with a never-firing trigger behaves like plain `delay`.
#[test]
fn legacy_delay_with_reset_never_resets() {
    let c = constant(7u64);
    // A trigger derived from the same graph that never ticks.
    let never = c.filter_value(|_| false);
    let delayed = c
        .delay_with_reset(Duration::from_nanos(50), &never)
        .accumulate();
    // constant ticks once at t=0; delayed re-emits it at t=50.
    delayed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![7], delayed.peek_value());
}

/// `split` decomposes a signal of pairs into two component signals.
#[test]
fn legacy_split_decomposes_pairs() {
    let pairs = ticker(Duration::from_nanos(100))
        .count()
        .map(|i: &u64| (*i, *i * 10));
    let (a, b) = pairs.split();
    let aa = a.accumulate();
    let bb = b.accumulate();
    // Running `aa` builds the shared graph (which includes `bb`); both read
    // off the same retained runner.
    aa.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], aa.peek_value());
    assert_eq!(vec![10, 20, 30], bb.peek_value());
}

/// `filter_none` drops `None`, yielding just the `Some` payloads.
#[test]
fn legacy_filter_none_drops_none() {
    // counts 1..=6; Some for odd inputs only → 1, 3, 5
    let out = ticker(Duration::from_nanos(100))
        .count()
        .map(|i: &u64| (i % 2 == 1).then_some(*i))
        .filter_none()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![1, 3, 5], out.peek_value());
}

// --- the surface the facade gained when its forwarding became generated ------
//
// These fifteen methods were missing while `Signal`'s combinators were
// hand-written: each landed in the op catalog, got its fluent method, and
// nobody came back to forward it. They exist now because `#[op(fluent)]`
// writes the `Signal` twin too, and these tests are what says the generated
// signatures are usable and agree with their fluent counterparts.

/// `join` combines two signals, ticking when *either* input ticks.
#[test]
fn legacy_join_combines_two_signals() {
    let count = ticker(Duration::from_nanos(100)).count();
    let doubled = count.map(|c: &u64| c * 2);
    let summed = count.join(&doubled, |a: &u64, b: &u64| a + b).accumulate();
    summed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![3, 6, 9], summed.peek_value());
}

/// `join_passive` reads `other` without letting it trigger the combine, so the
/// output ticks once per *source* tick — three ticks here, not the six a pair
/// of active edges would give.
///
/// The first pair is `(1, 1)`, not `(1, 0)`: `delay` writes its value slot with
/// `Tick::Silent`, updating the slot without ticking, precisely so a passive
/// reader never sees `T::default()`. That contract is only observable through a
/// passive edge, which makes this its test.
#[test]
fn legacy_join_passive_does_not_trigger_on_the_passive_edge() {
    let count = ticker(Duration::from_nanos(100)).count();
    let lagged = count.delay(Duration::from_nanos(100));
    let paired = count
        .join_passive(&lagged, |now: &u64, then: &u64| (*now, *then))
        .accumulate();
    paired.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![(1, 1), (2, 1), (3, 2)], paired.peek_value());
}

/// `join3` takes two further edges, all active.
#[test]
fn legacy_join3_combines_three_signals() {
    let count = ticker(Duration::from_nanos(100)).count();
    let doubled = count.map(|c: &u64| c * 2);
    let tripled = count.map(|c: &u64| c * 3);
    let summed = count
        .join3(&doubled, &tripled, |a: &u64, b: &u64, c: &u64| a + b + c)
        .accumulate();
    summed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![6, 12, 18], summed.peek_value());
}

/// The `try_` joins propagate a closure error out of `run`, aborting it — the
/// facade's fallibility contract reaching the generated methods.
#[test]
fn legacy_try_join_aborts_the_run_on_error() {
    let count = ticker(Duration::from_nanos(100)).count();
    let doubled = count.map(|c: &u64| c * 2);
    let checked = count.try_join(&doubled, |a: &u64, b: &u64| {
        if *a >= 3 {
            anyhow::bail!("too big");
        }
        Ok(a + b)
    });
    let err = checked.run(HISTORICAL, RunFor::Cycles(5)).unwrap_err();
    // The engine wraps the closure's error in node context, so the cause chain
    // (`{:#}`) is where the original message lives.
    assert!(format!("{err:#}").contains("too big"), "{err:#}");
    assert!(err.to_string().contains("TryJoin"), "{err}");
}

/// `try_join3` / `try_join_passive` wire the same way; assert the happy path
/// so both generated signatures stay exercised.
#[test]
fn legacy_try_join3_and_try_join_passive_combine() {
    let count = ticker(Duration::from_nanos(100)).count();
    let doubled = count.map(|c: &u64| c * 2);
    let tripled = count.map(|c: &u64| c * 3);
    let three = count
        .try_join3(&doubled, &tripled, |a: &u64, b: &u64, c: &u64| {
            Ok(a + b + c)
        })
        .accumulate();
    let passive = count
        .try_join_passive(&doubled, |a: &u64, b: &u64| Ok(a + b))
        .accumulate();
    three.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![6, 12, 18], three.peek_value());
    assert_eq!(vec![3, 6, 9], passive.peek_value());
}

/// `drop_small_change` suppresses a tick whose change the predicate calls
/// small, measured against the last *emitted* value — so a slow drift of
/// individually-small steps still eventually gets through.
#[test]
fn legacy_drop_small_change_suppresses_small_steps() {
    let values = ticker(Duration::from_nanos(100))
        .count()
        .map(|c: &u64| *c as f64)
        .drop_small_change(|last: &f64, next: &f64| (next - last).abs() < 2.5)
        .accumulate();
    values.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![1.0, 4.0], values.peek_value());
}

/// `logged` is a pass-through tap. It stays hand-written (its `&str` label
/// differs from the op's `(String, Level)` `Cfg`), so this pins that the
/// facade carries it at all — the gap that prompted generating the rest.
#[test]
fn legacy_logged_passes_through() {
    let counted = ticker(Duration::from_nanos(100))
        .count()
        .logged("count", log::Level::Debug)
        .accumulate();
    counted.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], counted.peek_value());
}

/// The generated `inspect` / `for_each` still see every value, and the
/// generated `sample` still reads passively — the three that carry side
/// effects or a passive mask, where a mis-generated body would show up as
/// wrong observations rather than a compile error.
#[test]
fn legacy_generated_taps_observe_every_value() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    // Every signal here comes off the one `ticker`: each free source function
    // mints its *own* graph, so signals from two of them cannot be combined.
    let tk = ticker(Duration::from_nanos(100));
    let count = tk.count();
    let tapped = count.inspect(move |v: &u64| {
        recorded.lock().expect("seen mutex poisoned").push(*v);
    });
    let trigger = tk.filter(&count.map(|i: &u64| i.is_multiple_of(2)));
    let sampled = tapped.sample(&trigger).accumulate();
    sampled.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(
        vec![1, 2, 3, 4, 5, 6],
        *seen.lock().expect("seen mutex poisoned"),
        "inspect sees every tick of its source"
    );
    assert_eq!(
        vec![2, 4, 6],
        sampled.peek_value(),
        "sample reads on the trigger's tick only"
    );
}
