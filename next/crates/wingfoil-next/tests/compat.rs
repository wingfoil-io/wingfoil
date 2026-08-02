//! Phase 6 facade: classic-style wingfoil programs run on the new engine.
//! These are written exactly as classic code is — free source functions,
//! `stream.run(...)`, `stream.peek_value()` — but every stream is backed by
//! the new `Op`/`Builder` engine. This is the compatibility surface that
//! lets existing code (and the Python bindings) migrate unchanged.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil_next::compat::{constant, ticker};
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// The canonical classic snippet: count a ticker and read the result.
#[test]
fn classic_counter_runs_on_the_new_engine() {
    let counted = ticker(Duration::from_nanos(100)).count();
    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());
}

/// A classic chain: ticker → count → map → filter → accumulate, run and read
/// off the accumulator, all in the classic idiom.
#[test]
fn classic_chain_maps_filters_accumulates() {
    let count = ticker(Duration::from_nanos(100)).count();
    let is_even = count.map(|i| i.is_multiple_of(2));
    let evens = count.filter(&is_even).accumulate();
    evens.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![2, 4, 6], evens.peek_value());
}

/// A classic fold (running sum) driven off a counter.
#[test]
fn classic_fold_sums() {
    let total = ticker(Duration::from_nanos(100))
        .count()
        .fold(0u64, |acc, v| *acc += v);
    total.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // 1 + 2 + 3 + 4
    assert_eq!(10, total.peek_value());
}

/// Classic `constant` + `delay`, matching the classic engine's timing.
#[test]
fn classic_constant_and_delay() {
    let delayed = constant(7u64).delay(Duration::from_nanos(50)).accumulate();
    delayed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // constant ticks once at t=0; delayed re-emits it at t=50.
    assert_eq!(vec![7], delayed.peek_value());
}

/// Peeking before the graph has run is a reachable user error. `peek_value`
/// mirrors the classic infallible signature (`-> T`), so it enforces the
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

// --- Expanded operator surface, all in the classic idiom -------------------

/// `limit` passes the first N values, then stays quiet.
#[test]
fn classic_limit_passes_first_n() {
    let count = ticker(Duration::from_nanos(100)).count();
    let limited = count.limit(3).accumulate();
    limited.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1, 2, 3], limited.peek_value());
}

/// `distinct` suppresses consecutive duplicates (emit on change only).
#[test]
fn classic_distinct_drops_repeats() {
    // counts 1..=6 mapped through integer halving: 0,1,1,2,2,3 -> distinct 0,1,2,3
    let count = ticker(Duration::from_nanos(100)).count();
    let stepped = count.map(|i| i / 2).distinct().accumulate();
    stepped.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![0, 1, 2, 3], stepped.peek_value());
}

/// `difference` emits `value - previous`, quiet on the first value.
#[test]
fn classic_difference_of_successive_values() {
    // squares 1,4,9,16 -> successive differences 3,5,7
    let count = ticker(Duration::from_nanos(100)).count();
    let diffs = count.map(|i| i * i).difference().accumulate();
    diffs.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![3, 5, 7], diffs.peek_value());
}

/// `not` negates each value — here a parity flag.
#[test]
fn classic_not_negates() {
    let count = ticker(Duration::from_nanos(100)).count();
    // is_even over counts 1,2,3,4 -> false,true,false,true; not -> true,false,true,false
    let flipped = count.map(|i| i.is_multiple_of(2)).not().accumulate();
    flipped.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![true, false, true, false], flipped.peek_value());
}

/// `merge` recombines two disjoint sub-streams of one source; exactly one
/// ticks each cycle, so the merge reconstructs the original sequence.
#[test]
fn classic_merge_recombines_disjoint_streams() {
    let count = ticker(Duration::from_nanos(100)).count();
    let evens = count.filter(&count.map(|i| i.is_multiple_of(2)));
    let odds = count.filter(&count.map(|i| !i.is_multiple_of(2)));
    let merged = odds.merge(&evens).accumulate();
    merged.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![1, 2, 3, 4], merged.peek_value());
}

/// `sample` reads the current value of one stream whenever a trigger ticks.
#[test]
fn classic_sample_reads_on_trigger() {
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
fn classic_with_time_pairs_time_and_value() {
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
fn classic_ticked_at_emits_engine_time() {
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
fn classic_ticked_at_elapsed_emits_elapsed() {
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
fn classic_throttle_rate_limits() {
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
fn classic_window_flushes_on_interval() {
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
fn classic_buffer_flushes_by_capacity() {
    let buffered = ticker(Duration::from_nanos(100))
        .count()
        .buffer(2)
        .accumulate();
    buffered.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![vec![1, 2], vec![3, 4], vec![5]], buffered.peek_value());
}

// --- Newly surfaced operator methods, all in the classic idiom -------------

/// `try_map` applies a fallible closure; the `Ok` path passes values through.
#[test]
fn classic_try_map_transforms_values() {
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
fn classic_map_filter_keeps_even_counts_with_times() {
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
fn classic_filter_map_keeps_some() {
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
fn classic_filter_value_drops_on_predicate() {
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
fn classic_reduce_running_sum() {
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
fn classic_produce_emits_on_each_tick() {
    let out = ticker(Duration::from_nanos(100))
        .produce(|| 42u64)
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![42, 42, 42], out.peek_value());
}

/// `inspect` runs a side effect while passing values through unchanged.
#[test]
fn classic_inspect_taps_and_passes_through() {
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
fn classic_for_each_sinks_each_value() {
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
fn classic_finally_runs_at_teardown() {
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
fn classic_collapse_takes_last_item() {
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
fn classic_timed_passes_through() {
    let out = ticker(Duration::from_nanos(100))
        .count()
        .timed()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], out.peek_value());
}

/// `print` passes values through unchanged (it only prints at teardown).
#[test]
fn classic_print_passes_through() {
    let out = ticker(Duration::from_nanos(100))
        .count()
        .print()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], out.peek_value());
}

/// `delay_with_reset` with a never-firing trigger behaves like plain `delay`.
#[test]
fn classic_delay_with_reset_never_resets() {
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
fn classic_split_decomposes_pairs() {
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
fn classic_filter_none_drops_none() {
    // counts 1..=6; Some for odd inputs only → 1, 3, 5
    let out = ticker(Duration::from_nanos(100))
        .count()
        .map(|i: &u64| (i % 2 == 1).then_some(*i))
        .filter_none()
        .accumulate();
    out.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![1, 3, 5], out.peek_value());
}
