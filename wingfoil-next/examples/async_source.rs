//! An async quote feed driving a wingfoil-next graph.
//!
//! A tokio task simulates a market data feed, pushing quotes into an
//! [`external`](wingfoil_next::fluent::GraphBuilder::external) source; each
//! send wakes the kernel, which runs one graph cycle. The graph maintains a
//! running mean and flags quotes that deviate from it.
//!
//! The graph thread and the async world touch only at the `ExternalSource`
//! handle — the graph itself stays single-threaded and lock-free.
//!
//! ```sh
//! cargo run -p wingfoil-next --example async_source
//! ```

use std::time::Duration;

use wingfoil::{RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;

fn main() {
    let g = GraphBuilder::new();
    let (quotes, feed) = g.external::<f64>();

    let mean = quotes
        .fold((0.0_f64, 0u64), |st, q| {
            st.0 += q;
            st.1 += 1;
        })
        .map(|(sum, n)| sum / *n as f64);

    let log = quotes
        .join(&mean, |q, m| {
            let dev = (q - m) / m * 100.0;
            let flag = if dev.abs() > 1.0 { "  <-- outlier" } else { "" };
            format!("quote {q:>7.2}  mean {m:>7.2}  dev {dev:>+5.2}%{flag}")
        })
        .accumulate();

    // The async producer: a tokio task sleeping between sends, like a feed
    // handler would await socket reads. `send` returns false once the runner
    // is gone, which stops the task.
    let producer = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build tokio runtime");
        rt.block_on(async move {
            let mut price = 100.0_f64;
            let mut lcg = 0x2545_F491_4F6C_DD1D_u64;
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
                lcg = lcg
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let unit = ((lcg >> 33) as f64 / (1u64 << 30) as f64) - 1.0; // [-1, 1)
                // Mostly small moves, occasionally a jump.
                price += if lcg.is_multiple_of(7) {
                    unit * 4.0
                } else {
                    unit * 0.3
                };
                if !feed.send(price) {
                    break; // runner finished
                }
            }
        });
    });

    // Each quote wakes the kernel for one cycle: 20 quotes, then stop.
    let g_runner = &mut g.build();
    g_runner.run(RunMode::RealTime, RunFor::Cycles(20));

    println!("processed {} quotes from the async feed:", 20);
    for line in g_runner.value(&log) {
        println!("  {line}");
    }
    producer.join().expect("producer thread");
}
