//! Parity tests for `spawn` / `spawn_map` — the thread-offload combinators
//! (next's twins of classic `producer()` / `mapper()`, the `graph_node` node).
//!
//! Historical assertions are exact (values *and* tick times); realtime
//! assertions check values only (wall-clock burst grouping is timing-dependent),
//! matching how classic's `graph_node_works` treats the two modes.
//!
//! The worker inherits the driving run's mode **and** bound (as classic
//! `producer` does), so a perpetual producer (a ticker) is bounded by the run —
//! these tests use bounded runs, never `Forever` with a ticker.

use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

/// `spawn`: a producer sub-graph (ticker → running count) runs on a worker
/// thread; the main graph scales each value ×10. Historical replay is fully
/// deterministic: one value per instant, delivered at the tick time.
#[test]
fn spawn_source_replays_deterministically_in_historical_mode() {
    let period = Duration::from_millis(10);
    let n = 6u64;

    let g = GraphBuilder::new();
    let counts: Stream<Burst<u64>> = g.spawn(move |wg| wg.ticker(period).count().limit(n as u32));
    let scaled = counts
        .map(|b: &Burst<u64>| b.iter().map(|x| x * 10).collect::<Vec<u64>>())
        .with_time();
    let collected = scaled.accumulate();
    let mut runner = g.build();

    // Bound generously past the producer's `limit`; historical replay is
    // wall-clock-free, so the worker races to its bound and closes at once.
    runner
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * (n as u32 + 3)),
        )
        .expect("historical spawn run");

    let out = runner.value(&collected);
    let times: Vec<u64> = out.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<Vec<u64>> = out.iter().map(|(_, v)| v.clone()).collect();

    // count() starts at 1; ×10 → 10,20,…,60. next's ticker fires at
    // 0,p,2p,… (see catalog.rs), so the six values land at 0..5p.
    assert_eq!(
        values,
        vec![vec![10], vec![20], vec![30], vec![40], vec![50], vec![60]]
    );
    let p = period.as_nanos() as u64;
    assert_eq!(times, vec![0, p, 2 * p, 3 * p, 4 * p, 5 * p]);
}

/// The same producer in realtime: values are correct; wall-clock pacing may
/// group several into a burst, so only the flattened value sequence is asserted.
#[test]
fn spawn_source_delivers_all_values_in_realtime() {
    let period = Duration::from_millis(2);
    let n = 6u64;

    let g = GraphBuilder::new();
    let counts: Stream<Burst<u64>> = g.spawn(move |wg| wg.ticker(period).count().limit(n as u32));
    let collected = counts.accumulate();
    let mut runner = g.build();

    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(80)),
        )
        .expect("realtime spawn run");

    let flat: Vec<u64> = runner.value(&collected).into_iter().flatten().collect();
    assert_eq!(flat, vec![1, 2, 3, 4, 5, 6]);
}

/// `spawn_map`: the input (ticker → running count) is mapped ×10 through a
/// sub-graph running on a worker thread. Both graphs run historical in lock-step
/// by graph time, so the result is fully deterministic — the capability the
/// incremental channel unblocked (block-collect deadlocked this).
#[test]
fn spawn_map_replays_deterministically_in_historical_mode() {
    let period = Duration::from_millis(10);
    let n = 5u64;

    let g = GraphBuilder::new();
    let input = g.ticker(period).count().limit(n as u32); // 1..5 at 0,p,2p,3p,4p
    let scaled: Stream<Burst<u64>> =
        input.spawn_map(|s: Stream<Burst<u64>>| s.map(|b: &Burst<u64>| b.iter().sum::<u64>() * 10));
    let collected = scaled
        .map(|b: &Burst<u64>| b.iter().copied().collect::<Vec<u64>>())
        .with_time()
        .accumulate();
    let mut runner = g.build();

    runner
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * (n as u32 + 3)),
        )
        .expect("historical spawn_map run");

    let out = runner.value(&collected);
    let times: Vec<u64> = out.iter().map(|(t, _)| u64::from(*t)).collect();
    let values: Vec<Vec<u64>> = out.iter().map(|(_, v)| v.clone()).collect();

    assert_eq!(
        values,
        vec![vec![10], vec![20], vec![30], vec![40], vec![50]]
    );
    let p = period.as_nanos() as u64;
    assert_eq!(times, vec![0, p, 2 * p, 3 * p, 4 * p]);
}

/// The same mapper in realtime: values correct, grouping timing-dependent.
#[test]
fn spawn_map_delivers_all_values_in_realtime() {
    let period = Duration::from_millis(2);
    let n = 5u64;

    let g = GraphBuilder::new();
    let input = g.ticker(period).count().limit(n as u32);
    let scaled: Stream<Burst<u64>> =
        input.spawn_map(|s: Stream<Burst<u64>>| s.map(|b: &Burst<u64>| b.iter().sum::<u64>() * 10));
    let collected = scaled.accumulate();
    let mut runner = g.build();

    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(80)),
        )
        .expect("realtime spawn_map run");

    let flat: Vec<u64> = runner.value(&collected).into_iter().flatten().collect();

    // The mapper sums each burst, and under realtime the grouping is timing
    // dependent (as the doc comment says) — two counter values landing in one
    // burst legitimately produce one summed output. Asserting
    // `[10, 20, 30, 40, 50]` therefore asserted a *grouping*, not the values,
    // and flaked on any runner that coalesced a pair: observed failures were
    // `[10, 20, 70, 50]` (3+4 together) and `[10, 20, 120]` (3+4+5).
    //
    // So check what the transport actually promises: every value arrives, in
    // order, grouped arbitrarily. Each output is `sum(group) * 10` over a
    // contiguous run of the input, so walking the expected counts and
    // consuming one run per output must reproduce the input exactly.
    let mut expected = 1..=n;
    for value in &flat {
        assert_eq!(
            0,
            value % 10,
            "each output is a burst sum scaled by 10: {flat:?}"
        );
        let mut remaining = value / 10;
        while remaining > 0 {
            let next = expected
                .next()
                .unwrap_or_else(|| panic!("more values delivered than sent: {flat:?}"));
            remaining = remaining.checked_sub(next).unwrap_or_else(|| {
                panic!("output {value} is not a run of consecutive counts: {flat:?}")
            });
        }
    }
    assert!(
        expected.next().is_none(),
        "not every value was delivered: {flat:?}"
    );
}

/// `spawn_bounded` / `spawn_map_bounded`: a small transport bound applies
/// back-pressure but never changes the delivered values or tick times.
#[test]
fn bounded_spawn_and_spawn_map_match_the_unbounded_result() {
    let period = Duration::from_millis(10);
    let n = 5u64;

    let g = GraphBuilder::new();
    // Bound the producer→graph channel to 2 (worker blocks if it gets >2 ahead).
    let counts: Stream<Burst<u64>> =
        g.spawn_bounded(Some(2), move |wg| wg.ticker(period).count().limit(n as u32));
    let flat = counts.map(|b: &Burst<u64>| b.iter().sum::<u64>());
    // Bound both worker channels to 2 as well.
    let scaled: Stream<Burst<u64>> = flat.spawn_map_bounded(Some(2), |s: Stream<Burst<u64>>| {
        s.map(|b: &Burst<u64>| b.iter().sum::<u64>() * 10)
    });
    let collected = scaled
        .map(|b: &Burst<u64>| b.iter().copied().collect::<Vec<u64>>())
        .with_time()
        .accumulate();
    let mut runner = g.build();

    runner
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * (n as u32 + 3)),
        )
        .expect("bounded historical run");

    let out = runner.value(&collected);
    let values: Vec<Vec<u64>> = out.iter().map(|(_, v)| v.clone()).collect();
    let times: Vec<u64> = out.iter().map(|(t, _)| u64::from(*t)).collect();
    assert_eq!(
        values,
        vec![vec![10], vec![20], vec![30], vec![40], vec![50]]
    );
    let p = period.as_nanos() as u64;
    assert_eq!(times, vec![0, p, 2 * p, 3 * p, 4 * p]);
}
