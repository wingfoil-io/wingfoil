//! kafka adapter example — produce messages, consume and transform, write back.
//!
//! A port of the classic `wingfoil/examples/kafka` example onto the next engine.
//! Two independent graph roots share one [`GraphBuilder`]:
//!
//! - `seed`       — writes source messages once via `constant` + `kafka_pub`.
//! - `round_trip` — consumes the source topic, uppercases each value, and writes
//!   the result to a destination topic.
//!
//! The graph owns the tokio runtime the adapters spawn on. This example builds
//! its own runtime and installs it as the override via `with_async_runtime`;
//! the simplest usage omits that and lets the graph create one lazily (see the
//! module docs).
//!
//! # Prerequisites
//!
//! A running Kafka-compatible broker (Redpanda recommended):
//! ```sh
//! docker run --rm -p 9092:9092 \
//!   docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
//!   redpanda start --overprovisioned --smp 1 --memory 512M \
//!   --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
//! ```
//!
//! # Run
//!
//! ```sh
//! cargo run -p wingfoil-next --example kafka_adapter --features kafka
//! ```

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::kafka::{KafkaEvent, KafkaRecord, KafkaSinkOps, kafka_sub};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

const BROKERS: &str = "localhost:9092";
const SOURCE_TOPIC: &str = "example-source";
const DEST_TOPIC: &str = "example-dest";

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    // The graph owns the runtime the adapters spawn on. Here we hand it our own
    // (the override); omit `.with_async_runtime(..)` to let the graph create and
    // own one lazily.
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());

    // Seed the source topic with two messages, once.
    let _seed = g
        .constant(burst![
            KafkaRecord {
                topic: SOURCE_TOPIC.into(),
                key: Some(b"greeting".to_vec()),
                value: b"hello".to_vec(),
            },
            KafkaRecord {
                topic: SOURCE_TOPIC.into(),
                key: Some(b"subject".to_vec()),
                value: b"world".to_vec(),
            },
        ])
        .kafka_pub(BROKERS)?;

    // Consume the source topic, uppercase each value, write to the dest topic.
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Cycles(3),
        start_time: NanoTime::ZERO,
    };
    let _round_trip = kafka_sub(&g, params, BROKERS, SOURCE_TOPIC, "example-group")?
        .map(|burst: &Burst<KafkaEvent>| {
            burst
                .iter()
                .map(|event| {
                    let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
                    println!(
                        "  {} → {}",
                        event.key_str().and_then(|r| r.ok()).unwrap_or("?"),
                        String::from_utf8_lossy(&upper)
                    );
                    KafkaRecord {
                        topic: DEST_TOPIC.into(),
                        key: event.key.clone(),
                        value: upper,
                    }
                })
                .collect::<Burst<KafkaRecord>>()
        })
        .kafka_pub(BROKERS)?;

    g.build().run(RunMode::RealTime, RunFor::Cycles(3))?;
    Ok(())
}
