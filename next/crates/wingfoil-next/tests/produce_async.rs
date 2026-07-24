//! Phase 3: the `produce_async` ergonomic — an async closure yielding
//! timestamped values, matching classic `produce_async`. Same producer runs
//! deterministically in historical mode (replayed on the graph clock) and
//! propagates a mid-stream error into the graph. Gated by the `async` feature.
#![cfg(feature = "async")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::{RunParams, produce_async, produce_async_bounded};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

fn params() -> RunParams {
    RunParams {
        run_mode: HISTORICAL,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    }
}

/// A finite async producer of timestamped values replays deterministically on
/// the graph clock — the classic `produce_async` historical contract.
#[test]
fn produce_async_replays_deterministically() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let values = produce_async(&g, rt.handle(), params(), |_p| async {
        Ok(futures::stream::iter(vec![
            Ok((NanoTime::new(100), 1u64)),
            Ok((NanoTime::new(200), 2u64)),
            Ok((NanoTime::new(300), 3u64)),
        ]))
    });
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
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let values = produce_async(&g, rt.handle(), params(), |_p| async {
        Ok(futures::stream::iter(vec![
            Ok((NanoTime::new(100), 1u64)),
            Ok((NanoTime::new(100), 2u64)),
            Ok((NanoTime::new(100), 3u64)),
        ]))
    });
    let acc = values.collapse_accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();
    assert_eq!(vec![1, 2, 3], r.value(&acc));
}

/// A mid-stream producer error aborts the run with context.
#[test]
fn produce_async_error_aborts_the_run() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let values = produce_async(&g, rt.handle(), params(), |_p| async {
        Ok(futures::stream::iter(vec![
            Ok((NanoTime::new(100), 1u64)),
            Err(anyhow::anyhow!("feed dropped")),
        ]))
    });
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

/// Caller-supplied `RunParams` that disagree with the actual run are rejected,
/// not silently trusted: declaring a historical `start_time` that the run does
/// not use aborts the run with context naming the mismatch.
#[test]
fn produce_async_rejects_mismatched_start_time() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    // Caller declares start_time = 999, but the run below starts at ZERO.
    let bogus = RunParams {
        run_mode: HISTORICAL,
        run_for: RunFor::Forever,
        start_time: NanoTime::new(999),
    };
    let values = produce_async(&g, rt.handle(), bogus, |_p| async {
        Ok(futures::stream::iter(vec![Ok((NanoTime::new(100), 1u64))]))
    });
    let _acc = values.collapse_accumulate();
    let mut r = g.build();
    let err = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("mismatched RunParams.start_time must abort the run");
    let msg = format!("{err:#}");
    assert!(msg.contains("start_time"), "names the mismatch: {msg}");
    assert!(
        msg.contains("does not match"),
        "explains the disagreement: {msg}"
    );
}

/// Matching params replay fine even when a `buffer_size` is supplied for a
/// historical run — the up-front collection is never throttled (no deadlock),
/// so the bound is simply not applied in historical replay.
#[test]
fn produce_async_buffer_size_ignored_in_historical() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let values = produce_async_bounded(&g, rt.handle(), params(), Some(1), |_p| async {
        Ok(futures::stream::iter(vec![
            Ok((NanoTime::new(100), 1u64)),
            Ok((NanoTime::new(200), 2u64)),
            Ok((NanoTime::new(300), 3u64)),
        ]))
    });
    let acc = values.collapse_accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();
    assert_eq!(vec![1, 2, 3], r.value(&acc));
}

/// A realtime producer with a small `buffer_size` still delivers every value in
/// order: the producer blocks once it is `buffer_size` values ahead and resumes
/// as the graph consumes and returns permits (back-pressure, not loss).
#[test]
fn produce_async_realtime_bounded_buffer_delivers_all_in_order() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();
    let realtime = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    };
    // buffer_size = 2, but ten values must all arrive across the run.
    let values = produce_async_bounded(&g, rt.handle(), realtime, Some(2), |_p| async {
        Ok(futures::stream::unfold(1u64, |i| async move {
            if i > 10 {
                return None;
            }
            // A small pace so the graph gets cycles to consume and release
            // permits, mirroring the cross-thread channel tests.
            tokio::time::sleep(Duration::from_millis(1)).await;
            Some((Ok((NanoTime::new(i), i)), i + 1))
        }))
    });
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
