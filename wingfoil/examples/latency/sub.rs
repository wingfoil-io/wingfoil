// Latency-instrumented iceoryx2 subscriber.
//
// Subscribes to the stream produced by `latency_pub`, stamps the `receive`,
// `strategy`, and `ack` stages, and aggregates per-stage delta statistics
// via `LatencyReport`. Press Ctrl-C to stop; the report is printed on
// shutdown.
//
// Run with:
//   cargo run --example latency_sub --features iceoryx2-beta

#[path = "shared.rs"]
mod shared;

use wingfoil::adapters::iceoryx2::iceoryx2_sub;
use wingfoil::*;

use shared::{Quote, QuoteLatency, SERVICE_NAME, quote_latency};

fn main() {
    env_logger::init();

    println!(
        "Subscribing to \"{SERVICE_NAME}\" — waiting for publisher... \
         (Ctrl-C to print report and exit)"
    );

    // The subscriber yields a `Burst<Traced<Quote, QuoteLatency>>` per tick.
    // collapse() drops empty bursts and unwraps single-message bursts to the
    // inner Traced value.
    let pipeline = iceoryx2_sub::<Traced<Quote, QuoteLatency>>(SERVICE_NAME)
        .collapse::<Traced<Quote, QuoteLatency>>()
        .stamp::<quote_latency::receive>()
        // ── pretend strategy work happens here ─────────────────────
        .map(|mut t: Traced<Quote, QuoteLatency>| {
            // Touch the payload so the compiler can't fold the work away.
            t.payload.price *= 1.0001;
            t
        })
        .stamp::<quote_latency::strategy>()
        // ── pretend reply construction happens here ────────────────
        .stamp::<quote_latency::ack>();

    // LatencyReport sink prints the per-stage delta histogram on shutdown.
    let (sink, _stats) = pipeline.latency_report(true);

    sink.run(RunMode::RealTime, RunFor::Forever).unwrap();
}
