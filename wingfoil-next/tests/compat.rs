//! Phase 6 facade: classic-style wingfoil programs run on the new engine.
//! These are written exactly as classic code is — free source functions,
//! `stream.run(...)`, `stream.peek_value()` — but every stream is backed by
//! the new `Op`/`Builder` engine. This is the compatibility surface that
//! lets existing code (and the Python bindings) migrate unchanged.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::compat::{Signal, constant, ticker};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A classic `f64` counter: 1.0, 2.0, 3.0, ... one value per 100ns tick.
fn counter_f64() -> Signal<f64> {
    ticker(Duration::from_nanos(100)).count().map(|i| *i as f64)
}

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

/// Re-running is deferred: the builder is consumed by the first run, so a
/// second `run` must surface a clear error instead of silently running an
/// empty graph and then panicking out-of-bounds in `peek_value`. The first
/// run's result stays readable afterwards.
#[test]
fn second_run_errors_and_leaves_first_result_intact() {
    let counted = ticker(Duration::from_nanos(100)).count();
    counted.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, counted.peek_value());

    let err = counted
        .run(HISTORICAL, RunFor::Cycles(5))
        .expect_err("a second run must error, not silently run an empty graph");
    assert!(
        err.to_string().contains("called more than once"),
        "unexpected error message: {err}"
    );

    // The first run's runner is untouched, so peek still returns its value.
    assert_eq!(5, counted.peek_value());
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

// --- Phase-6 facade expansion: transforms, joins, sinks, statistics ---------

/// `try_map` applies a fallible closure; the `Ok` path passes values through.
#[test]
fn classic_try_map_ok_path_passes_values() {
    let count = ticker(Duration::from_nanos(100)).count();
    let doubled = count
        .try_map(|i| Ok::<u64, anyhow::Error>(i * 2))
        .accumulate();
    doubled.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    assert_eq!(vec![2, 4, 6, 8], doubled.peek_value());
}

/// `try_map` returning `Err` aborts the run with context.
#[test]
fn classic_try_map_err_aborts_run() {
    let count = ticker(Duration::from_nanos(100)).count();
    let checked = count.try_map(|i| {
        if *i >= 3 {
            anyhow::bail!("value too large: {i}");
        }
        Ok(*i)
    });
    let err = checked
        .run(HISTORICAL, RunFor::Cycles(5))
        .expect_err("try_map Err must abort the run");
    let msg = format!("{err:#}");
    assert!(msg.contains("too large"), "unexpected error: {msg}");
}

/// `map_filter` maps and filters in one pass: `(value, emit?)`.
#[test]
fn classic_map_filter_maps_and_filters() {
    // counts 1..=6 -> keep evens, emit their halves: 2->1, 4->2, 6->3
    let count = ticker(Duration::from_nanos(100)).count();
    let kept = count
        .map_filter(|i| (i / 2, i.is_multiple_of(2)))
        .accumulate();
    kept.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![1, 2, 3], kept.peek_value());
}

/// `join` combines two active signals, ticking when either ticks.
#[test]
fn classic_join_combines_two_signals() {
    let count = ticker(Duration::from_nanos(100)).count();
    let tens = count.map(|i| i * 10);
    let summed = count.join(&tens, |a, b| a + b).accumulate();
    summed.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // count i and 10*i tick together each cycle: i + 10i = 11i
    assert_eq!(vec![11, 22, 33, 44], summed.peek_value());
}

/// `join_passive` triggers on the left signal; the right is read passively.
#[test]
fn classic_join_passive_reads_right_passively() {
    let tk = ticker(Duration::from_nanos(100));
    let count = tk.count();
    let value = count.map(|i| i * 100);
    // trigger fires on even counts only (cycles 2,4,6)
    let trigger = tk.filter(&count.map(|i| i.is_multiple_of(2)));
    let sampled = trigger.join_passive(&value, |_, v| *v).accumulate();
    sampled.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![200, 400, 600], sampled.peek_value());
}

/// `join3` combines three active signals.
#[test]
fn classic_join3_combines_three_signals() {
    let count = ticker(Duration::from_nanos(100)).count();
    let tens = count.map(|i| i * 10);
    let hundreds = count.map(|i| i * 100);
    let summed = count
        .join3(&tens, &hundreds, |a, b, c| a + b + c)
        .accumulate();
    summed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // i + 10i + 100i = 111i
    assert_eq!(vec![111, 222, 333], summed.peek_value());
}

/// `try_join` combines two signals via a fallible closure (Ok path).
#[test]
fn classic_try_join_ok_path() {
    let count = ticker(Duration::from_nanos(100)).count();
    let tens = count.map(|i| i * 10);
    let summed = count
        .try_join(&tens, |a, b| Ok::<u64, anyhow::Error>(a + b))
        .accumulate();
    summed.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![11, 22, 33], summed.peek_value());
}

/// `map_n` chains the same map `n` times.
#[test]
fn classic_map_n_chains_map() {
    let count = ticker(Duration::from_nanos(100)).count();
    // add 1 three times: i -> i + 3
    let bumped = count.map_n(3, |i| i + 1).accumulate();
    bumped.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![4, 5, 6], bumped.peek_value());
}

/// `inspect` observes each value and passes it through unchanged.
#[test]
fn classic_inspect_taps_values() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen_tap = seen.clone();
    let count = ticker(Duration::from_nanos(100)).count();
    let tapped = count
        .inspect(move |v| seen_tap.borrow_mut().push(*v))
        .accumulate();
    tapped.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], tapped.peek_value());
    assert_eq!(vec![1, 2, 3], *seen.borrow());
}

/// `for_each` runs a fallible sink on each tick.
#[test]
fn classic_for_each_runs_sink() {
    let sum = Rc::new(RefCell::new(0u64));
    let sum_sink = sum.clone();
    let count = ticker(Duration::from_nanos(100)).count();
    let sink = count.for_each(move |v| {
        *sum_sink.borrow_mut() += *v;
        Ok(())
    });
    sink.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // 1 + 2 + 3 + 4
    assert_eq!(10, *sum.borrow());
}

/// `finally` runs once at teardown, observing the last value.
#[test]
fn classic_finally_runs_at_teardown() {
    let last = Rc::new(RefCell::new(0u64));
    let last_sink = last.clone();
    let count = ticker(Duration::from_nanos(100)).count();
    let done = count.finally(move |v| {
        *last_sink.borrow_mut() = *v;
        Ok(())
    });
    done.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(5, *last.borrow());
}

/// `fan` splits into parallel branches and merges them back. Here two disjoint
/// branches (evens, odds) reconstruct the original sequence.
#[test]
fn classic_fan_splits_and_merges() {
    let count = ticker(Duration::from_nanos(100)).count();
    let fanned = count
        .fan(2, |branch: Signal<u64>| {
            let cond = branch.map(|i| i.is_multiple_of(2));
            branch.filter(&cond)
        })
        .accumulate();
    fanned.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // both branches filter to evens, merge is idempotent here -> 2, 4
    assert_eq!(vec![2, 4], fanned.peek_value());
}

/// `rolling_sum` over the f64 counter: window 3 of 1,2,3,4,5.
#[test]
fn classic_rolling_sum_window() {
    let sums = counter_f64().rolling_sum(3).accumulate();
    sums.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    // windows: {1},{1,2},{1,2,3},{2,3,4},{3,4,5} -> 1,3,6,9,12
    assert_eq!(vec![1.0, 3.0, 6.0, 9.0, 12.0], sums.peek_value());
}

/// `rolling_mean` over the f64 counter: window 2.
#[test]
fn classic_rolling_mean_window() {
    let means = counter_f64().rolling_mean(2).accumulate();
    means.run(HISTORICAL, RunFor::Cycles(4)).unwrap();
    // windows: {1},{1,2},{2,3},{3,4} -> 1, 1.5, 2.5, 3.5
    assert_eq!(vec![1.0, 1.5, 2.5, 3.5], means.peek_value());
}

/// `ewma_per_tick` seeds on the first sample then blends with `alpha`.
#[test]
fn classic_ewma_per_tick_blends() {
    let ewma = counter_f64().ewma_per_tick(0.5).accumulate();
    ewma.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    // seed 1; then 0.5*2 + 0.5*1 = 1.5; then 0.5*3 + 0.5*1.5 = 2.25
    assert_eq!(vec![1.0, 1.5, 2.25], ewma.peek_value());
}
