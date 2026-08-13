//! Multi-threaded graph execution: a producer sub-graph runs on its own
//! worker thread and feeds the main graph through the channel layer.
//!
//! This is the wingfoil port of the legacy `threading` example. Legacy
//! offers `producer()` / `mapper()` combinators that run a sub-graph on a
//! dedicated thread and shuttle values over channels. Wingfoil does not put those
//! combinators in the fluent vocabulary — instead it exposes the primitive they
//! were built on directly: a [`channel`](wingfoil::fluent::SourceOps::channel)
//! source plus a `Send` [`ChannelSender`]. A worker thread builds and runs its
//! *own* graph and forwards each value through the sender; the main graph
//! receives a [`Burst`] of everything that arrived since its last cycle. The
//! graphs stay single-threaded and lock-free; they touch only at the channel.
//!
//! A second, third, … stage chains the same way: each is a worker thread whose
//! graph has a channel *in* and a `ChannelSender` *out*. Here the "mapper"
//! stage (scale ×10) runs as an ordinary op on the receiving main graph.
//!
//! It runs in **both** modes. In realtime, values arrive whenever the worker
//! produces them, so a cycle may carry a burst of several. In a historical
//! replay the worker sends timestamped values ([`ChannelSender::send_at`]) and
//! the receiver replays them **deterministically** at those graph times — same
//! topology, reproducible timeline.
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example threading
//! ```

use std::thread;
use std::time::Duration;

use wingfoil::channel::ChannelSender;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// Run the producer-on-a-worker-thread pipeline in `run_mode`, printing each
/// burst of scaled values under `label` as the main graph receives it.
fn pipeline(label: &str, run_mode: RunMode, period: Duration, n: u32) {
    let g = GraphBuilder::new();

    // The main graph's inbound edge: a channel source fed from the worker.
    let (counts, to_main): (Stream<Burst<u64>>, ChannelSender<u64>) = g.channel::<u64>();

    // The "mapper" stage, on the main graph: scale every value in each burst
    // ×10 (legacy runs this on its own thread via `mapper()`; on wingfoil a stage
    // that needs no thread of its own is just an op on the receiving graph).
    let scaled = counts.map(|b: &Burst<u64>| b.iter().map(|x| x * 10).collect::<Vec<u64>>());

    // Print each burst as the main graph receives it — the burst grouping *is*
    // the thing this example shows, and a sink shows it live rather than
    // collecting the whole run into a `Vec` first.
    let label = label.to_string();
    let _printed = scaled.for_each(move |burst: &Vec<u64>| {
        println!("{label}: {burst:?}");
        Ok(())
    });

    let mut runner = g.build();

    // The "producer" stage, on a worker thread: it builds and runs its OWN
    // wingfoil graph (a ticker → running count) and forwards every value
    // over the channel — wingfoil's replacement for legacy `producer()`.
    let worker = thread::spawn(move || {
        let wg = GraphBuilder::new();
        // `with_time` stamps each value: `send_at` replays deterministically at
        // graph time in a historical run and is treated as a plain `send` (the
        // stamp ignored) in realtime.
        let ticks = wg.ticker(period).count().with_time();
        let send = to_main.clone();
        let _forward = ticks.for_each(move |tv: &(NanoTime, u64)| {
            send.send_at(tv.1, tv.0);
            Ok(())
        });
        let mut wrunner = wg.build();
        wrunner
            .run(run_mode, RunFor::Cycles(n))
            .expect("run producer sub-graph");
        // End-of-stream: the main graph stops once it drains the final burst.
        to_main.close();
    });

    // `Forever` + the worker's `close()` bounds the run: the channel source ends
    // it on end-of-stream rather than a fixed cycle count (which realtime burst
    // coalescing would make unreliable).
    runner
        .run(run_mode, RunFor::Forever)
        .expect("run main graph");
    worker.join().expect("producer worker thread");
}

fn main() {
    let period = Duration::from_millis(20);
    let n = 6;

    // Realtime: the worker paces at `period`; a cycle may carry a burst of
    // several values, so the grouping is wall-clock dependent.
    pipeline("realtime  ", RunMode::RealTime, period, n);

    // Historical: timestamped replay — one value per instant, fully
    // deterministic: [10], [20], [30], [40], [50], [60].
    pipeline(
        "historical",
        RunMode::HistoricalFrom(NanoTime::ZERO),
        period,
        n,
    );
}
