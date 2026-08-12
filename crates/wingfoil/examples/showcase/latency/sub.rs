//! Latency-instrumented iceoryx2 subscriber (wingfoil).
//!
//! Subscribes to the stream produced by `latency_pub`, stamps the `receive`,
//! `strategy`, and `ack` stages, and aggregates per-stage delta statistics via
//! `latency_report`. Press Ctrl-C to stop; the report is printed on shutdown.
//!
//! A port of the legacy `legacy/wingfoil/examples/latency/sub.rs` onto the wingfoil
//! engine. This is the cross-process proof for the Phase-5 latency
//! infrastructure: the stamps written in one process are read back and
//! differenced in another, with `Traced<Quote, QuoteLatency>` riding shared
//! memory as a plain `#[repr(C)]` payload.
//!
//! # Run
//!
//! ```sh
//! # run until interrupted
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example latency_sub --features iceoryx2
//! # run for a bounded 10s, then print the report and exit
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example latency_sub --features iceoryx2 -- 10
//! ```
//!
//! # Deviation from legacy: the optional run duration
//!
//! Legacy's subscriber runs [`RunFor::Forever`] only, and its README says to
//! stop with Ctrl-C to get the report — but `print_on_teardown` fires from the
//! graph's teardown, which a `SIGINT` never reaches, so on that path the report
//! is never printed. Passing a duration runs the graph to a clean stop, so
//! teardown runs and the report is emitted. Omit the argument for legacy's
//! run-forever behaviour.

#[path = "shared.rs"]
mod shared;

use std::time::Duration;

use wingfoil::adapters::iceoryx2::iceoryx2_sub;
use wingfoil::latency::{LatencyBurstStreamOps, LatencyReportOps, Traced};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

use shared::{Quote, QuoteLatency, SERVICE_NAME, quote_latency};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // An optional run duration in seconds; without it we run forever (legacy
    // parity) and the teardown-time report is only reachable via a clean stop.
    let run_for = match std::env::args().nth(1) {
        Some(secs) => {
            let secs: u64 = secs
                .parse()
                .map_err(|_| anyhow::anyhow!("expected a run duration in seconds, got {secs:?}"))?;
            RunFor::Duration(Duration::from_secs(secs))
        }
        None => RunFor::Forever,
    };

    match run_for {
        RunFor::Duration(d) => println!(
            "Subscribing to \"{SERVICE_NAME}\" for {d:?} — waiting for publisher... \
             (report prints on exit)"
        ),
        _ => println!(
            "Subscribing to \"{SERVICE_NAME}\" — waiting for publisher... \
             (pass a duration in seconds to get the report on exit)"
        ),
    }

    let g = GraphBuilder::new();

    // The subscriber yields a `Burst<Traced<Quote, QuoteLatency>>` per cycle,
    // and the pipeline stays burst-shaped. A quote stream is latest-wins for a
    // *strategy*, so `collapse()` would be a defensible way to price off it —
    // but this pipeline exists to measure latency, and each value in a burst
    // is a *sample*. `collapse()` keeps only the burst's last value, and
    // bursts only grow when the publisher outruns a graph cycle, so it would
    // silently drop exactly the samples taken while the system was busy —
    // biasing the very histogram this example prints.
    let pipeline =
        iceoryx2_sub::<Traced<Quote, QuoteLatency>>(&g, RunMode::RealTime, SERVICE_NAME)?
            .stamp_each::<quote_latency::receive>()
            // ── pretend strategy work happens here ─────────────────────
            .map(|ts: &Burst<Traced<Quote, QuoteLatency>>| {
                // Touch each payload so the compiler can't fold the work away.
                let mut ts = ts.clone();
                for t in ts.iter_mut() {
                    t.payload.price *= 1.0001;
                }
                ts
            })
            .stamp_each::<quote_latency::strategy>()
            // ── pretend reply construction happens here ────────────────
            .stamp_each::<quote_latency::ack>();

    // The latency_report sink prints the per-stage delta histogram on shutdown.
    let (_sink, _stats) = pipeline.latency_report(true);

    g.build().run(RunMode::RealTime, run_for)?;
    Ok(())
}
