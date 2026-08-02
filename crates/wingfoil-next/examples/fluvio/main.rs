//! fluvio adapter example — publish records to a topic, consume and transform
//! them, write the results to a second topic.
//!
//! A port of the classic `wingfoil/examples/fluvio` example onto the next
//! engine. Two independent graph roots share one [`GraphBuilder`]:
//!
//! - `seed`      — writes source records once via `constant` + `fluvio_pub`.
//! - `transform` — consumes the source topic, uppercases each value, and writes
//!   the result to the destination topic.
//!
//! The graph owns the tokio runtime the adapters spawn on. This example builds
//! its own runtime and installs it as the override via `with_async_runtime`;
//! the simplest usage omits that and lets the graph create one lazily (see the
//! module docs).
//!
//! # Prerequisites
//!
//! A running Fluvio cluster with the source and destination topics created:
//!
//! ```sh
//! # Start a local cluster (requires the Fluvio CLI)
//! fluvio cluster start --local
//!
//! # Create topics
//! fluvio topic create fluvio-example-source
//! fluvio topic create fluvio-example-dest
//! ```
//!
//! # Run
//!
//! ```sh
//! cargo run -p wingfoil-next --example fluvio_adapter --features fluvio
//! ```

use wingfoil_next::adapters::fluvio::{FluvioEvent, FluvioRecord, FluvioSinkOps, fluvio_sub};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

const ENDPOINT: &str = "127.0.0.1:9003";
const SOURCE_TOPIC: &str = "fluvio-example-source";
const DEST_TOPIC: &str = "fluvio-example-dest";

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    // The graph owns the runtime the adapters spawn on. Here we hand it our own
    // (the override); omit `.with_async_runtime(..)` to let the graph create and
    // own one lazily.
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());

    // Root 1: seed two records into the source topic.
    let _seed = g
        .constant(burst![
            FluvioRecord::with_key("greeting", b"hello".to_vec()),
            FluvioRecord::with_key("subject", b"world".to_vec()),
        ])
        .fluvio_pub(ENDPOINT, SOURCE_TOPIC, None)?;

    // Root 2: consume the source topic, uppercase each value, write to the dest
    // topic.
    let _transform = fluvio_sub(&g, RunMode::RealTime, ENDPOINT, SOURCE_TOPIC, 0, None)?
        .map(|burst: &Burst<FluvioEvent>| {
            burst
                .iter()
                .map(|event| {
                    let key = event
                        .key_str()
                        .and_then(|r| r.ok())
                        .unwrap_or("")
                        .to_string();
                    let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
                    println!("  {} → {}", key, String::from_utf8_lossy(&upper));
                    FluvioRecord::with_key(key, upper)
                })
                .collect::<Burst<FluvioRecord>>()
        })
        .fluvio_pub(ENDPOINT, DEST_TOPIC, None)?;

    g.build().run(RunMode::RealTime, RunFor::Cycles(3))?;
    Ok(())
}
