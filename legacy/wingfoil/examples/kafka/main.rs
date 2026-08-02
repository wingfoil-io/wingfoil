//! Kafka adapter example — produce messages, consume and transform, write back.
//!
//! Two independent graph roots:
//! - `seed`        — writes source messages once via `constant` + `kafka_pub`
//! - `round_trip`  — consumes the source topic, uppercases values, writes to dest topic
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
//! cargo run --example kafka --features kafka
//! ```

use wingfoil::adapters::kafka::*;
use wingfoil::*;

const BROKERS: &str = "localhost:9092";
const SOURCE_TOPIC: &str = "example-source";
const DEST_TOPIC: &str = "example-dest";

fn main() -> anyhow::Result<()> {
    let conn = KafkaConnection::new(BROKERS);

    let seed = constant(burst![
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
    .kafka_pub(conn.clone());

    let round_trip = kafka_sub(conn.clone(), SOURCE_TOPIC, "example-group")
        .map(|burst| {
            burst
                .into_iter()
                .map(|event| {
                    let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
                    println!(
                        "  {} → {}",
                        event.key_str().and_then(|r| r.ok()).unwrap_or("?"),
                        String::from_utf8_lossy(&upper)
                    );
                    KafkaRecord {
                        topic: DEST_TOPIC.into(),
                        key: event.key,
                        value: upper,
                    }
                })
                .collect::<Burst<KafkaRecord>>()
        })
        .kafka_pub(conn);

    Graph::new(vec![seed, round_trip], RunMode::RealTime, RunFor::Cycles(3)).run()?;

    Ok(())
}
