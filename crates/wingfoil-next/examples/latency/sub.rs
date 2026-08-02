//! Latency-instrumented iceoryx2 subscriber (wingfoil-next).
//!
//! Subscribes to the stream produced by `latency_pub`, stamps the `receive`,
//! `strategy`, and `ack` stages, and aggregates per-stage delta statistics via
//! `latency_report`. Press Ctrl-C to stop; the report is printed on shutdown.
//!
//! A port of the classic `wingfoil/examples/latency/sub.rs` onto the next
//! engine. This is the cross-process proof for the Phase-5 latency
//! infrastructure: the stamps written in one process are read back and
//! differenced in another, with `Traced<Quote, QuoteLatency>` riding shared
//! memory as a plain `#[repr(C)]` payload.
//!
//! # Run
//!
//! ```sh
//! # run until interrupted
//! cargo run -p wingfoil-next --example latency_sub --features iceoryx2
//! # run for a bounded 10s, then print the report and exit
//! cargo run -p wingfoil-next --example latency_sub --features iceoryx2 -- 10
//! ```
//!
//! # Deviation from classic: the optional run duration
//!
//! Classic's subscriber runs [`RunFor::Forever`] only, and its README says to
//! stop with Ctrl-C to get the report — but `print_on_teardown` fires from the
//! graph's teardown, which a `SIGINT` never reaches, so on that path the report
//! is never printed. Passing a duration runs the graph to a clean stop, so
//! teardown runs and the report is emitted. Omit the argument for classic's
//! run-forever behaviour.

#[path = "shared.rs"]
mod shared;

use std::time::Duration;

use wingfoil_next::adapters::iceoryx2::iceoryx2_sub;
use wingfoil_next::latency::{LatencyReportOps, LatencyStreamOps, Traced};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

use shared::{Quote, QuoteLatency, SERVICE_NAME, quote_latency};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // An optional run duration in seconds; without it we run forever (classic
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

    // The subscriber yields a `Burst<Traced<Quote, QuoteLatency>>` per cycle.
    // collapse() drops empty bursts and unwraps single-message bursts to the
    // inner Traced value.
    let pipeline =
        iceoryx2_sub::<Traced<Quote, QuoteLatency>>(&g, RunMode::RealTime, SERVICE_NAME)?
            .collapse::<Traced<Quote, QuoteLatency>>()
            .stamp::<quote_latency::receive>()
            // ── pretend strategy work happens here ─────────────────────
            .map(|t: &Traced<Quote, QuoteLatency>| {
                // Touch the payload so the compiler can't fold the work away.
                let mut t = *t;
                t.payload.price *= 1.0001;
                t
            })
            .stamp::<quote_latency::strategy>()
            // ── pretend reply construction happens here ────────────────
            .stamp::<quote_latency::ack>();

    // The latency_report sink prints the per-stage delta histogram on shutdown.
    let (_sink, _stats) = pipeline.latency_report(true);

    g.build().run(RunMode::RealTime, run_for)?;
    Ok(())
}
