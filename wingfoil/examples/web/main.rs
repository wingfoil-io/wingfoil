//! End-to-end example for the wingfoil `web` adapter.
//!
//! Runs a [`WebServer`] that publishes a synthetic mid price on topic
//! `"price"` and listens for UI events on topic `"ui"`. Open
//! `ws://localhost:8080/ws` in a browser (or point the `wingfoil-js`
//! sample UI at it) to watch the stream.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example web --features web
//! ```
//!
//! # Historical streaming (replaying a backtest to the browser)
//!
//! Set `WINGFOIL_WEB_HISTORICAL=1` to run the same graph in
//! [`RunMode::HistoricalFrom`] instead of real time. The publisher then
//! replays a finite series (500 points) with a small per-point delay that
//! stands in for a genuinely slow computation, so the browser can follow
//! the replay live. When the series is exhausted the client receives a
//! `Complete` control frame (surfaced by `@wingfoil/client` as
//! `onComplete`) and the run ends.
//!
//! ```sh
//! WINGFOIL_WEB_HISTORICAL=1 cargo run --example web --features web
//! ```
//!
//! The only server-side difference is the run mode and using `.start()`
//! (not `.start_historical()`, which would make the adapter a no-op).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use wingfoil::adapters::web::*;
use wingfoil::*;

/// Payload type published on topic "price".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PriceTick {
    mid: f64,
    count: u64,
}

/// Payload type received from the browser on topic "ui".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UiEvent {
    kind: String,
    note: String,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let addr = std::env::var("WINGFOIL_WEB_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let historical = std::env::var("WINGFOIL_WEB_HISTORICAL").is_ok();

    // `.start()` streams under both run modes. `.start_historical()` is the
    // no-op path for a backtest that does *not* want a server — not used here
    // because we specifically want to stream the replay to the browser.
    let server = WebServer::bind(&addr).start()?;
    let port = server.port();
    let codec = server.codec();
    let mode = if historical { "historical" } else { "realtime" };
    println!(
        "wingfoil web example listening on ws://127.0.0.1:{port}/ws  (codec: {codec:?}, mode: {mode})"
    );

    // Publish a synthetic mid-price stream. In real time this ticks at
    // 100 Hz forever; in historical mode it replays a finite 500-point
    // series with a small per-point delay standing in for a slow
    // computation, so the browser can watch the replay unfold.
    let price_stream = ticker(Duration::from_millis(10))
        .count()
        .map(move |n| {
            if historical {
                // Simulate a slow per-point computation so the replay is
                // paced for a human watching in the browser rather than
                // flushing 500 points in microseconds.
                std::thread::sleep(Duration::from_millis(20));
            }
            PriceTick {
                // Wobble around 100 with a tiny sine.
                mid: 100.0 + ((n as f64) * 0.1).sin(),
                count: n,
            }
        })
        .web_pub(&server, "price");

    // Receive browser UI events and log them.
    let ui_events: std::rc::Rc<dyn Stream<Burst<UiEvent>>> = web_sub(&server, "ui");
    let ui_log = ui_events.collapse().for_each(|event, time| {
        println!("{} ui-event: {:?}", time.pretty(), event);
    });

    // Real time runs forever (Ctrl-C to stop); historical replays a finite
    // series and then ends, at which point clients receive `Complete`.
    let (run_mode, run_for) = if historical {
        (RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(500))
    } else {
        (RunMode::RealTime, RunFor::Forever)
    };

    Graph::new(vec![price_stream, ui_log], run_mode, run_for).run()?;

    Ok(())
}
