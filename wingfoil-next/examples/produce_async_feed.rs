//! `produce_async`: an async producer of timestamped values driving a graph,
//! run the same way in **both** modes. Run with the `async` feature:
//!
//! ```sh
//! cargo run -p wingfoil-next --features async --example produce_async_feed
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::{RunParams, produce_async};
use wingfoil_next::fluent::GraphBuilder;

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new();

    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(NanoTime::ZERO),
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    };

    // A producer that awaits (like a socket read) and yields timestamped
    // quotes — a finite feed that closes when exhausted.
    let quotes = produce_async(&g, rt.handle(), params, |_p| async {
        Ok(futures::stream::unfold(
            (0u32, 100.0_f64),
            |(i, price)| async move {
                if i >= 8 {
                    return None;
                }
                // Await, as a real feed handler would on a socket.
                tokio::time::sleep(Duration::from_millis(1)).await;
                let price = price + (i as f64 % 3.0) - 1.0;
                let t = NanoTime::new(100 * (i as u64 + 1));
                Some((Ok((t, price)), (i + 1, price)))
            },
        ))
    });

    let mean = quotes
        .collapse_accumulate()
        .map(|qs| qs.iter().sum::<f64>() / qs.len().max(1) as f64);

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect("run");

    println!("running mean of async feed: {:.3}", runner.value(&mean));
}
