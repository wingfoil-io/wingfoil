//! redis Pub/Sub adapter example — publish, subscribe, transform, republish.
//!
//! A port of the classic `wingfoil/examples/redis` example onto the next engine.
//! Redis Pub/Sub is fire-and-forget: a subscriber only sees messages published
//! after its `SUBSCRIBE` completes. This example therefore wires three roles as
//! separate graphs (each on its own thread with its own tokio runtime) and starts
//! the subscribers before the publisher sends anything:
//!
//! - **processor** — subscribes to `source`, uppercases each payload, republishes
//!   to `dest`.
//! - **verifier**  — subscribes to `dest` and prints what it receives.
//! - **publisher** — publishes two messages to `source`.
//!
//! The graph owns the tokio runtime the adapters spawn on — created lazily, so
//! no `&Handle` is threaded in (see the module docs). Because the sinks drive
//! writes with `Handle::block_on`, each graph is built, run, and dropped from
//! its (non-async) thread.
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
//! cargo run -p wingfoil-next --example redis_adapter --features redis
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::redis::{RedisConnection, RedisEntry, RedisSinkOps, redis_sub};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

const URL: &str = "redis://127.0.0.1:6379";
const SOURCE: &str = "example-source";
const DEST: &str = "example-dest";

/// A realtime run description lasting `secs` seconds.
fn realtime(secs: u64) -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Duration(Duration::from_secs(secs)),
        start_time: NanoTime::ZERO,
    }
}

fn main() -> anyhow::Result<()> {
    let conn = RedisConnection::new(URL);

    // processor: source -> uppercase -> dest
    let processor_conn = conn.clone();
    let processor = std::thread::spawn(move || -> anyhow::Result<()> {
        // The graph owns the tokio runtime (created lazily); no `&Handle` to pass.
        let g = GraphBuilder::new();
        let _sink = redis_sub(&g, realtime(2).run_mode, processor_conn.clone(), SOURCE)?
            .map(|burst: &Burst<_>| {
                burst
                    .iter()
                    .map(|event: &wingfoil_next::adapters::redis::RedisEvent| {
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
            .redis_pub(processor_conn, None)?;
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
        Ok(())
    });

    // verifier: print everything that lands on dest
    let verifier_conn = conn.clone();
    let verifier = std::thread::spawn(move || -> anyhow::Result<()> {
        // The graph owns the tokio runtime (created lazily); no `&Handle` to pass.
        let g = GraphBuilder::new();
        let _print = redis_sub(&g, realtime(2).run_mode, verifier_conn, DEST)?
            .collapse()
            .for_each(|event: &wingfoil_next::adapters::redis::RedisEvent| {
                println!(
                    "  {} -> {}",
                    event.channel,
                    event.payload_str().unwrap_or("?")
                );
                Ok(())
            });
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
        Ok(())
    });

    // Give both subscribers time to register before publishing.
    std::thread::sleep(Duration::from_millis(500));

    // The graph owns the tokio runtime (created lazily); no `&Handle` to pass.
    let g = GraphBuilder::new();
    let _pub = g
        .constant(burst![
            RedisEntry {
                channel: SOURCE.into(),
                payload: b"hello".to_vec(),
            },
            RedisEntry {
                channel: SOURCE.into(),
                payload: b"world".to_vec(),
            },
        ])
        .redis_pub(conn, None)?;
    g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;

    processor.join().expect("processor thread panicked")?;
    verifier.join().expect("verifier thread panicked")?;
    Ok(())
}
