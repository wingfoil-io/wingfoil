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
    let server = WebServer::bind(&addr).start()?;
    let port = server.port();
    let codec = server.codec();
    println!("wingfoil web example listening on ws://127.0.0.1:{port}/ws  (codec: {codec:?})");

    // Publish a synthetic mid-price stream at 100 Hz.
    let price_stream = ticker(Duration::from_millis(10))
        .count()
        .map(|n| PriceTick {
            // Wobble around 100 with a tiny sine.
            mid: 100.0 + ((n as f64) * 0.1).sin(),
            count: n,
        })
        .web_pub(&server, "price");

    // Receive browser UI events and log them.
    let ui_events: std::rc::Rc<dyn Stream<Burst<UiEvent>>> = web_sub(&server, "ui");
    let ui_log = ui_events.collapse().for_each(|event, time| {
        println!("{} ui-event: {:?}", time.pretty(), event);
    });

    // Run both the publisher and the subscriber together. The server stays
    // up until the graph is stopped (Ctrl-C).
    Graph::new(
        vec![price_stream, ui_log],
        RunMode::RealTime,
        RunFor::Forever,
    )
    .run()?;

    Ok(())
}
