//! Phase 3: the `produce_async` ergonomic — an async closure yielding
//! timestamped values, matching classic `produce_async`. Same producer runs
//! deterministically in historical mode (replayed on the graph clock) and
//! propagates a mid-stream error into the graph. Gated by the `async` feature.
#![cfg(feature = "async")]

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::{RunParams, produce_async};
use wingfoil_next::fluent::GraphBuilder;

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
