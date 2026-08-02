//! Latency-instrumented iceoryx2 publisher (wingfoil-next).
//!
//! Publishes a `Traced<Quote, QuoteLatency>` over shared memory, stamping the
//! `produce` and `publish` stages on the way out. The subscriber stamps
//! `receive`, `strategy`, and `ack` on the way in and prints per-stage delta
//! statistics on shutdown.
//!
//! A port of the legacy `legacy/wingfoil/examples/latency/pub.rs` onto the next
//! engine. The stamping surface is identical (`.stamp::<stage>()`); only the
//! wiring is next-idiomatic — a `GraphBuilder` plus the `Iceoryx2SinkOps`
//! extension trait instead of the legacy free `iceoryx2_pub` function.
//!
//! # Run
//!
//! Start the subscriber first so it doesn't miss messages:
//!
//! ```sh
//! # Terminal 1
//! cargo run -p wingfoil-next --example latency_sub --features iceoryx2
//! # Terminal 2
//! cargo run -p wingfoil-next --example latency_pub --features iceoryx2
//! ```

#[path = "shared.rs"]
mod shared;

use std::time::Duration;

use wingfoil_next::adapters::iceoryx2::Iceoryx2SinkOps;
use wingfoil_next::latency::{LatencyStreamOps, Traced};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

use shared::{Quote, QuoteLatency, SERVICE_NAME, quote_latency};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let period = Duration::from_millis(100);

    let g = GraphBuilder::new();

    // produce → publish:
    //   `produce` is stamped immediately after the message is constructed,
    //   `publish` is stamped just before it crosses the iceoryx2 boundary.
    let _publisher = g
        .ticker(period)
        .count()
        .map(|seq: &u64| {
            Traced::<Quote, QuoteLatency>::new(Quote {
                seq: *seq,
                price: 100.0 + (*seq as f64) * 0.01,
            })
        })
        .stamp::<quote_latency::produce>()
        .stamp::<quote_latency::publish>()
        // The sink takes a `Burst<T>`; wrap each sample.
        .map(|t: &Traced<Quote, QuoteLatency>| burst![*t])
        .iceoryx2_pub(SERVICE_NAME);

    println!(
        "Publishing Traced<Quote, QuoteLatency> on \"{SERVICE_NAME}\" every {period:?} \
         — press Ctrl-C to stop"
    );
    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}
