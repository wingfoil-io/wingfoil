//! Redis Pub/Sub adapter example — publish, subscribe, transform, republish.
//!
//! Redis Pub/Sub is fire-and-forget: a subscriber only sees messages published
//! after its `SUBSCRIBE` completes. This example therefore wires three roles as
//! separate graphs and starts the subscribers before the publisher sends anything:
//!
//! - **processor** — subscribes to `source`, uppercases each payload, republishes to `dest`
//! - **verifier**  — subscribes to `dest` and prints what it receives
//! - **publisher** — publishes two messages to `source`
//!
//! # Prerequisites
//!
//! A running Redis:
//! ```sh
//! docker run --rm -p 6379:6379 redis:7-alpine
//! ```
//!
//! # Run
//!
//! ```sh
//! cargo run --example redis --features redis
//! ```

use std::time::Duration;

use wingfoil::adapters::redis::*;
use wingfoil::*;

const URL: &str = "redis://127.0.0.1:6379";
const SOURCE: &str = "example-source";
const DEST: &str = "example-dest";

fn main() -> anyhow::Result<()> {
    let conn = RedisConnection::new(URL);

    // processor: source -> uppercase -> dest
    let processor_conn = conn.clone();
    let processor = std::thread::spawn(move || -> anyhow::Result<()> {
        redis_sub(processor_conn.clone(), SOURCE)
            .map(|burst| {
                burst
                    .into_iter()
                    .map(|event| {
                        let upper = event
                            .payload_str()
                            .unwrap_or("")
                            .to_uppercase()
                            .into_bytes();
                        RedisEntry {
                            channel: DEST.to_string(),
                            payload: upper,
                        }
                    })
                    .collect::<Burst<RedisEntry>>()
            })
            .redis_pub(processor_conn)
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
        Ok(())
    });

    // verifier: print everything that lands on dest
    let verifier_conn = conn.clone();
    let verifier = std::thread::spawn(move || -> anyhow::Result<()> {
        redis_sub(verifier_conn, DEST)
            .collapse()
            .for_each(|event, _| {
                println!(
                    "  {} -> {}",
                    event.channel,
                    event.payload_str().unwrap_or("?")
                );
            })
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
        Ok(())
    });

    // Give both subscribers time to register before publishing.
    std::thread::sleep(Duration::from_millis(500));

    constant(burst![
        RedisEntry {
            channel: SOURCE.into(),
            payload: b"hello".to_vec()
        },
        RedisEntry {
            channel: SOURCE.into(),
            payload: b"world".to_vec()
        },
    ])
    .redis_pub(conn)
    .run(RunMode::RealTime, RunFor::Cycles(1))?;

    processor.join().expect("processor thread panicked")?;
    verifier.join().expect("verifier thread panicked")?;
    Ok(())
}
