//! Fluvio adapter example — publish records to a topic, consume and transform them.
//!
//! Two independent graph roots:
//! - `seed`        — writes source records once via `constant` + `fluvio_pub`
//! - `transform`   — reads from the source topic, uppercases values, writes to dest topic
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
//! Or start the SC in Docker and create topics via the admin API.
//!
//! # Run
//!
//! ```sh
//! cargo run --example fluvio --features fluvio
//! ```

use wingfoil::adapters::fluvio::*;
use wingfoil::*;

const ENDPOINT: &str = "127.0.0.1:9003";
const SOURCE_TOPIC: &str = "fluvio-example-source";
const DEST_TOPIC: &str = "fluvio-example-dest";

fn main() -> anyhow::Result<()> {
    let conn = FluvioConnection::new(ENDPOINT);

    // Root 1: seed two records into the source topic.
    let seed = constant(burst![
        FluvioRecord::with_key("greeting", b"hello".to_vec()),
        FluvioRecord::with_key("subject", b"world".to_vec()),
    ])
    .fluvio_pub(conn.clone(), SOURCE_TOPIC);

    // Root 2: subscribe to source topic, transform values to uppercase, write to dest topic.
    let transform = fluvio_sub(conn.clone(), SOURCE_TOPIC, 0, None)
        .map(|burst| {
            burst
                .into_iter()
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
        .fluvio_pub(conn, DEST_TOPIC);

    Graph::new(vec![seed, transform], RunMode::RealTime, RunFor::Cycles(3)).run()?;

    Ok(())
}
