//! Phase 6 facade: classic-style wingfoil programs run on the new engine.
//! These are written exactly as classic code is — free source functions,
//! `stream.run(...)`, `stream.peek_value()` — but every stream is backed by
//! the new `Op`/`Builder` engine. This is the compatibility surface that
//! lets existing code (and the Python bindings) migrate unchanged.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::compat::{constant, ticker};

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
