//! Phase 3: the `produce_async` ergonomic — an async closure yielding
//! timestamped values, matching classic `produce_async`. Same producer runs
//! deterministically in historical mode (replayed on the graph clock) and
//! propagates a mid-stream error into the graph. Gated by the `async` feature.
//!
//! The runtime is the **graph's** (owned lazily, dropped at teardown), so no
//! `&Handle` is threaded in; the final test pins the caller-override escape hatch.
#![cfg(feature = "async")]

use std::time::Duration;

use wingfoil_next::async_source::produce_async;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A finite async producer of timestamped values replays deterministically on
/// the graph clock — the classic `produce_async` historical contract.
#[test]
fn produce_async_replays_deterministically() {
    let g = GraphBuilder::new();
    let values = produce_async(
        &g,
        |_p| async {
            Ok(futures::stream::iter(vec![
                Ok((NanoTime::new(100), 1u64)),
                Ok((NanoTime::new(200), 2u64)),
                Ok((NanoTime::new(300), 3u64)),
            ]))
        },
        None,
    )
    .unwrap();
    let acc = values.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();

    let got: Vec<(NanoTime, Vec<u64>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        vec![
            (NanoTime::new(100), vec![1]),
            (NanoTime::new(200), vec![2]),
            (NanoTime::new(300), vec![3]),
        ],
        got
    );
}

/// Same-timestamp values from the producer arrive as one atomic burst.
#[test]
fn produce_async_groups_same_time_into_a_burst() {
    let g = GraphBuilder::new();
    let values = produce_async(
        &g,
        |_p| async {
            Ok(futures::stream::iter(vec![
                Ok((NanoTime::new(100), 1u64)),
                Ok((NanoTime::new(100), 2u64)),
                Ok((NanoTime::new(100), 3u64)),
            ]))
        },
        None,
    )
    .unwrap();
    let acc = values.collapse_accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();
    assert_eq!(vec![1, 2, 3], r.value(&acc));
}

/// A mid-stream producer error aborts the run with context.
#[test]
fn produce_async_error_aborts_the_run() {
    let g = GraphBuilder::new();
    let values = produce_async(
        &g,
        |_p| async {
            Ok(futures::stream::iter(vec![
                Ok((NanoTime::new(100), 1u64)),
                Err(anyhow::anyhow!("feed dropped")),
            ]))
        },
        None,
    )
    .unwrap();
    let _acc = values.collapse_accumulate();
    let mut r = g.build();
    let err = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("a producer error must abort the run");
    assert!(
        format!("{err:#}").contains("feed dropped"),
        "cause: {err:#}"
    );
}

/// A bounded historical replay produces byte-identical values *and* tick times
/// to the unbounded one — back-pressure paces the producer (it fetches the next
/// group only as the graph drains) without changing what the graph sees. Covers
/// the floor: `Some(1)` behaves as `Some(2)` and must not deadlock.
#[test]
fn produce_async_bounded_historical_is_deterministic() {
    fn run_with(buffer: Option<usize>) -> Vec<(NanoTime, Vec<u64>)> {
        let g = GraphBuilder::new();
        let values = produce_async(
            &g,
            |_p| async {
                Ok(futures::stream::iter(
                    (1u64..=20).map(|i| Ok((NanoTime::new(i * 100), i))),
                ))
            },
            buffer,
        )
        .unwrap();
        let acc = values.with_time().accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Forever).unwrap();
        r.value(&acc)
            .into_iter()
            .map(|(t, b)| (t, b.iter().copied().collect()))
            .collect()
    }
    let unbounded = run_with(None);
    assert_eq!(20, unbounded.len());
    assert_eq!(unbounded, run_with(Some(5)));
    assert_eq!(unbounded, run_with(Some(2)));
    assert_eq!(
        unbounded,
        run_with(Some(1)),
        "Some(1) floors to 2, no deadlock"
    );
}

/// A same-time burst **larger than the bound** must not deadlock. Historical
/// back-pressure counts timestamp-*groups*, so an arbitrarily large same-time
/// burst rides one permit and is sent whole before the producer waits — the fix
/// over a naive per-value permit, which would stall mid-burst on a permit only
/// the group's own (blocked) delivery could release.
#[test]
fn produce_async_bounded_large_same_time_burst_no_deadlock() {
    let g = GraphBuilder::new();
    let values = produce_async(
        &g,
        |_p| async {
            // 10 values all at t=100 (one group, far larger than the bound of 2),
            // then a later group so the first can be closed and delivered.
            let mut items: Vec<_> = (0..10u64).map(|i| Ok((NanoTime::new(100), i))).collect();
            items.push(Ok((NanoTime::new(200), 99)));
            Ok(futures::stream::iter(items))
        },
        Some(2),
    )
    .unwrap();
    let acc = values.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();
    let got: Vec<(NanoTime, Vec<u64>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        vec![
            (NanoTime::new(100), vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
            (NanoTime::new(200), vec![99]),
        ],
        got
    );
}

/// A realtime producer with a small `buffer_size` still delivers every value in
/// order: the producer blocks once it is `buffer_size` values ahead and resumes
/// as the graph consumes and returns permits (back-pressure, not loss).
#[test]
fn produce_async_realtime_bounded_buffer_delivers_all_in_order() {
    let g = GraphBuilder::new();
    // buffer_size = 2, but ten values must all arrive across the run.
    let values = produce_async(
        &g,
        |_p| async {
            Ok(futures::stream::unfold(1u64, |i| async move {
                if i > 10 {
                    return None;
                }
                // A small pace so the graph gets cycles to consume and release
                // permits, mirroring the cross-thread channel tests.
                tokio::time::sleep(Duration::from_millis(1)).await;
                Some((Ok((NanoTime::new(i), i)), i + 1))
            }))
        },
        Some(2),
    )
    .unwrap();
    let acc = values.collapse_accumulate();
    let mut r = g.build();
    // Generous cycle bound: with a burst source nothing is dropped, so all ten
    // arrive across however many cycles the scheduler grants.
    r.run(RunMode::RealTime, RunFor::Cycles(500)).unwrap();

    let got = r.value(&acc);
    assert_eq!(
        (1..=10).collect::<Vec<u64>>(),
        got,
        "all values, in order, under back-pressure"
    );
}

/// The producer is established at `start()`, not at wiring: the async closure
/// does not run until `run()` is called. Wiring stays side-effect-free (an
/// adapter's connect/subscribe no longer fires during graph construction) —
/// deviation-register A1. The sleep gives a would-be wiring-spawned producer
/// ample time to run, so this fails against the old spawn-at-wiring model.
#[test]
fn produce_async_defers_producer_to_run() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let started = Arc::new(AtomicBool::new(false));
    let started_producer = started.clone();
    let g = GraphBuilder::new();
    let values = produce_async(
        &g,
        move |_p| async move {
            started_producer.store(true, Ordering::SeqCst);
            Ok(futures::stream::iter(vec![Ok((NanoTime::new(100), 1u64))]))
        },
        None,
    )
    .unwrap();
    let acc = values.collapse_accumulate();
    let mut r = g.build();

    // After wiring + build, the producer must not have run. The graph owns a live
    // runtime by now, so a wiring-spawned task would execute during this sleep;
    // the deferral means no task exists until `start()`.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !started.load(Ordering::SeqCst),
        "producer must not run until run() — wiring stays side-effect-free"
    );

    r.run(HISTORICAL, RunFor::Forever).unwrap();
    assert!(
        started.load(Ordering::SeqCst),
        "producer runs once the graph starts"
    );
    assert_eq!(vec![1u64], r.value(&acc));
}

/// The caller-runtime override: a producer wired onto a graph built with
/// [`GraphBuilder::with_async_runtime`] spawns on the caller's runtime, not a
/// graph-created one — the escape hatch for embedding in an existing async app.
#[test]
fn produce_async_honours_caller_runtime_override() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let values = produce_async(
        &g,
        |_p| async {
            Ok(futures::stream::iter(vec![
                Ok((NanoTime::new(100), 1u64)),
                Ok((NanoTime::new(200), 2u64)),
            ]))
        },
        None,
    )
    .unwrap();
    let acc = values.collapse_accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();
    assert_eq!(vec![1, 2], r.value(&acc));
}
