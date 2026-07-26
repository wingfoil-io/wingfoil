//! Phase 3/4: the `consume_async` ergonomic — the sink counterpart of
//! `produce_async`. Each burst is drained to a background tokio task over a
//! bounded channel (back-pressure), a single consumer preserves write order,
//! and a write error propagates back into the graph and aborts the run. Gated
//! by the `async` feature.
#![cfg(feature = "async")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::{consume_async, consume_async_bursts};
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
        let (sink, flush) = consume_async(&g, None, move |v: u64| {
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
        let _sink = g
            .constant(burst![1u64, 2, 3, 4, 5])
            .for_each(sink)
            .finally(flush);
        let mut r = g.build();
        // The `flush` teardown drains every queued write before `run` returns.
        r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
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
        let (sink, flush) = consume_async(&g, Some(2), move |v: u64| {
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
        let _sink = g.constant(burst).for_each(sink).finally(flush);
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
    let (sink, flush) = consume_async(&g, Some(1), move |v: u64| {
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
    let _sink = stream.for_each(sink).finally(flush);

    let mut r = g.build();
    let err = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("a background write error must abort the run");
    assert!(
        format!("{err:#}").contains("sink write blew up"),
        "the run aborts with the write error as context: {err:#}",
    );
}

/// The **final** cycle's write error has no later cycle to surface it — only the
/// `flush` teardown can. A single value on a one-cycle run whose write fails must
/// still abort the run, via `finally(flush)` joining the consumer at teardown and
/// propagating its error. This is the generic form of etcd's `force:false`
/// conditional aborting a `RunFor::Cycles(1)` run (deviation-register B1), and the
/// reason `flush` exists rather than a plain `Drop`.
#[test]
fn consume_async_final_write_error_surfaces_at_teardown() {
    let g = GraphBuilder::new();
    // A single value, delivered on the only cycle. Its write fails in the
    // background task after the cycle has already returned `Ok`, so the sole path
    // to surfacing it is the teardown flush.
    let (sink, flush) = consume_async(&g, None, move |v: u64| async move {
        anyhow::bail!("final-cycle write blew up on {v}");
    })
    .unwrap();
    let _sink = g.constant(burst![1u64]).for_each(sink).finally(flush);

    let mut r = g.build();
    let err = r
        .run(HISTORICAL, RunFor::Cycles(1))
        .expect_err("a final-cycle write error must abort the run via the flush teardown");
    assert!(
        format!("{err:#}").contains("final-cycle write blew up"),
        "the flush teardown surfaces the final write error: {err:#}",
    );
}

/// Without the `flush` teardown wired, the `Drop` safety-net still drains every
/// queued write (nothing is lost) — it just cannot surface the final error (a
/// `Drop` returns no `Result`). The sink closure lives in a graph node owned by
/// the `Runner`, which drops its nodes *before* the async runtime (see
/// `AsyncRuntimeSlot`), so the `Drop`-time `block_on` drain runs on a live
/// runtime. Proves the fallback path drains rather than dropping in-flight writes.
#[test]
fn consume_async_drop_safety_net_still_drains_without_flush() {
    let collected = Arc::new(Mutex::new(Vec::<u64>::new()));
    {
        let sink_values = collected.clone();
        let g = GraphBuilder::new();
        let (sink, flush) = consume_async(&g, None, move |v: u64| {
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
        // Deliberately discard `flush` rather than wiring it — the caller forgot.
        // The sink node's Drop safety-net must still drain every queued write.
        drop(flush);
        let _sink = g.constant(burst![10u64, 20, 30]).for_each(sink);
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(1)).unwrap();
        // `r` drops here → the sink node (holding the last shared handle) drops
        // before the runtime, so the Drop safety-net drains the queued writes.
    }
    assert_eq!(
        vec![10, 20, 30],
        *collected.lock().expect("sink values mutex poisoned"),
        "the Drop safety-net drains all queued writes even without an explicit flush",
    );
}

/// `consume_async_bursts` hands the consumer a whole burst at a time (an owned
/// `Vec<T>`), so a sink can process a burst's values concurrently, while the
/// single consumer still preserves order *across* bursts. Two same-instant groups
/// arrive as two distinct `Vec`s, in order — never flattened, never reordered.
#[test]
fn consume_async_bursts_delivers_whole_bursts_in_order() {
    let collected = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
    {
        let sink_batches = collected.clone();
        let g = GraphBuilder::new();
        let (sink, flush) = consume_async_bursts(&g, None, move |batch: Vec<u64>| {
            let sink_batches = sink_batches.clone();
            async move {
                sink_batches
                    .lock()
                    .expect("sink batches mutex poisoned")
                    .push(batch);
                Ok(())
            }
        })
        .unwrap();

        // A historical channel groups same-instant `send_at` values into one
        // burst: {1,2,3} at t=100, then {4,5} at t=200.
        let (stream, sender) = g.channel::<u64>();
        for v in [1u64, 2, 3] {
            sender.send_at(v, NanoTime::new(100));
        }
        for v in [4u64, 5] {
            sender.send_at(v, NanoTime::new(200));
        }
        sender.close();

        let _sink = stream.for_each(sink).finally(flush);
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Forever).unwrap();
    }
    assert_eq!(
        vec![vec![1, 2, 3], vec![4, 5]],
        *collected.lock().expect("sink batches mutex poisoned"),
        "each burst arrives as one whole Vec, grouped by instant and in order",
    );
}
