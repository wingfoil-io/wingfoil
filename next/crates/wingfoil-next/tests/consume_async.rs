//! Phase 3/4: the `consume_async` ergonomic — the sink counterpart of
//! `produce_async`. Each burst is drained to a background tokio task over a
//! bounded channel (back-pressure), a single consumer preserves write order,
//! and a write error propagates back into the graph and aborts the run. Gated
//! by the `async` feature.
#![cfg(feature = "async")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::consume_async;
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A finite sink drains every value in the exact order the graph produced it —
/// the single-consumer ordering guarantee — and the teardown flush ensures all
/// queued writes have landed before the run returns.
#[test]
fn consume_async_preserves_order() {
    let collected = Arc::new(Mutex::new(Vec::<u64>::new()));

    {
        let sink_values = collected.clone();
        let g = GraphBuilder::new();
        // Unbounded buffer: every item is enqueued in one cycle, then drained in
        // order by the single consumer task.
        let consume = consume_async(&g, None, move |v: u64| {
            let sink_values = sink_values.clone();
            async move {
                sink_values
                    .lock()
                    .expect("sink values mutex poisoned")
                    .push(v);
                Ok(())
            }
        })
        .unwrap();
        let _sink = g.constant(burst![1u64, 2, 3, 4, 5]).for_each(consume);
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
        // `r` drops here → the sink flushes (drains every queued write) before
        // the scope ends.
    }

    assert_eq!(
        vec![1, 2, 3, 4, 5],
        *collected.lock().expect("sink values mutex poisoned"),
        "single consumer preserves write order; teardown flushes all writes",
    );
}

/// A small `buffer_size` bounds how far the graph runs ahead of a slow sink: the
/// sink closure blocks the graph thread on a full channel and resumes as the
/// consumer drains, so every value still arrives, in order, under back-pressure
/// (nothing dropped, no deadlock).
#[test]
fn consume_async_applies_backpressure() {
    let collected = Arc::new(Mutex::new(Vec::<u64>::new()));

    {
        let sink_values = collected.clone();
        let g = GraphBuilder::new();
        // Twenty values through a bounded channel of 2, with a slow sink so the
        // channel keeps filling and the graph thread blocks on `send` until the
        // consumer drains — exercising the back-pressure path.
        let consume = consume_async(&g, Some(2), move |v: u64| {
            let sink_values = sink_values.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                sink_values
                    .lock()
                    .expect("sink values mutex poisoned")
                    .push(v);
                Ok(())
            }
        })
        .unwrap();
        let values: Vec<u64> = (1..=20).collect();
        let mut burst = wingfoil::Burst::new();
        for v in values {
            burst.push(v);
        }
        let _sink = g.constant(burst).for_each(consume);
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
    }

    assert_eq!(
        (1..=20).collect::<Vec<u64>>(),
        *collected.lock().expect("sink values mutex poisoned"),
        "all values, in order, under back-pressure",
    );
}

/// A background write error propagates into the graph and aborts the run with
/// context. Values arrive across successive cycles (a historical channel
/// source); the erroring write closes the sink so a later cycle's send surfaces
/// the failure deterministically.
#[test]
fn consume_async_error_aborts_the_run() {
    let collected = Arc::new(Mutex::new(Vec::<u64>::new()));

    let sink_values = collected.clone();
    let g = GraphBuilder::new();
    let (stream, sender) = g.channel::<u64>();
    // One value per cycle at distinct historical instants.
    sender.send_at(1, NanoTime::new(100));
    sender.send_at(2, NanoTime::new(200));
    sender.send_at(3, NanoTime::new(300));
    sender.send_at(4, NanoTime::new(400));
    sender.send_at(5, NanoTime::new(500));
    sender.close();

    // buffer_size = 1 so a send blocks until the consumer takes the prior value;
    // once the consumer errors on `2` and stops, a later send hits the closed
    // channel and the run aborts — no reliance on async timing.
    let consume = consume_async(&g, Some(1), move |v: u64| {
        let sink_values = sink_values.clone();
        async move {
            if v == 2 {
                anyhow::bail!("sink write blew up on {v}");
            }
            sink_values
                .lock()
                .expect("sink values mutex poisoned")
                .push(v);
            Ok(())
        }
    })
    .unwrap();
    let _sink = stream.for_each(consume);

    let mut r = g.build();
    let err = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("a background write error must abort the run");
    assert!(
        format!("{err:#}").contains("sink write blew up"),
        "the run aborts with the write error as context: {err:#}",
    );
}
