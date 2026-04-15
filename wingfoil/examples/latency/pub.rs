// Latency-instrumented iceoryx2 publisher.
//
// Publishes a `Traced<Quote, QuoteLatency>` over shared memory, stamping the
// `produce` and `publish` stages on the way out. The subscriber stamps
// `receive`, `strategy`, and `ack` on the way in and prints per-stage delta
// statistics on shutdown.
//
// Run with:
//   cargo run --example latency_pub --features iceoryx2-beta
//
// Then in another terminal:
//   cargo run --example latency_sub --features iceoryx2-beta
//
// NOTE: iceoryx2 support is beta. Enable with the `iceoryx2-beta` feature flag.

#[path = "shared.rs"]
mod shared;

use std::time::Duration;
use wingfoil::adapters::iceoryx2::iceoryx2_pub;
use wingfoil::*;

use shared::{Quote, QuoteLatency, SERVICE_NAME, quote_latency};

fn main() {
    env_logger::init();

    let period = Duration::from_millis(100);

    // produce → publish:
    //   `produce` is stamped immediately after the message is constructed,
    //   `publish` is stamped just before it crosses the iceoryx2 boundary.
    let stream = ticker(period)
        .count()
        .map(|seq: u64| {
            Traced::<Quote, QuoteLatency>::new(Quote {
                seq,
                price: 100.0 + (seq as f64) * 0.01,
            })
        })
        .stamp::<quote_latency::produce>()
        .stamp::<quote_latency::publish>();

    // iceoryx2_pub takes a `Burst<T>`; wrap each sample.
    let burst_stream = stream.map(|t| {
        let mut b = Burst::default();
        b.push(t);
        b
    });

    let pub_node = iceoryx2_pub(burst_stream, SERVICE_NAME);

    println!(
        "Publishing Traced<Quote, QuoteLatency> on \"{SERVICE_NAME}\" every {period:?} \
         — press Ctrl-C to stop"
    );
    Graph::new(vec![pub_node], RunMode::RealTime, RunFor::Forever)
        .run()
        .unwrap();
}
