//! async / tokio integration: an async producer of *timestamped* values driving
//! a wingfoil-next graph — the classic `produce_async` model, ported to next.
//!
//! Async streams are a natural fit for IO but an awkward one for business
//! logic: their execution is implicit and depth-first. Wingfoil's is explicit,
//! breadth-first and time-aware (historical *and* realtime). The
//! [`produce_async`] bridge keeps the best of both: IO lives in the async
//! producer, business logic lives in the graph, and the boundary between them
//! is a single typed edge.
//!
//! [`produce_async`] maps an async [`futures::Stream`] of `(NanoTime, T)` onto
//! a graph source; the graph itself is the consumer (classic hands the stream
//! to an async `consume_async` closure — on next, an on-graph `for_each` plays
//! that role, keeping the consumer in the explicitly-timed world). The producer
//! runs on the caller's tokio runtime and each value wakes the kernel.
//!
//! Gated behind the `async` feature (tokio + futures):
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil-next --features async --example async
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::{RunParams, produce_async};
use wingfoil_next::prelude::*;

fn main() {
    // The graph owns the tokio runtime (created lazily); no `&Handle` to pass.
    let g = GraphBuilder::new();

    let period = Duration::from_millis(10);

    // The params describe the run we are about to drive (they must match the
    // `run(..)` below; a historical `start_time` mismatch is rejected at run
    // time — see the `produce_async` docs).
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    };

    // The async producer: it awaits between yields (as a socket read would),
    // emitting timestamped values. A finite feed of 8 values that ends —
    // closing the stream, which stops the graph.
    let quotes = produce_async(&g, params, move |_p| async move {
        Ok(futures::stream::unfold(0u32, move |i| async move {
            if i >= 8 {
                return None;
            }
            tokio::time::sleep(period).await; // simulate waiting IO
            let value = i * 10;
            // In realtime the timestamp is informational (the value ticks on
            // arrival); a historical producer would drive replay off it.
            Some((Ok((NanoTime::now(), value)), i + 1))
        }))
    })
    .expect("produce_async");

    // The consumer, on the graph: print every value in each arriving burst.
    let _printed = quotes.for_each(|burst: &Burst<u32>| {
        for value in burst.iter() {
            println!("{value}");
        }
        Ok(())
    });

    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Forever)
        .expect("run graph");
}
