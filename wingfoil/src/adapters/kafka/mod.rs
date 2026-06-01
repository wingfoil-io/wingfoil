//! Kafka adapter — streaming reads (consume) and writes (produce) for Apache Kafka.
//!
//! Provides two graph nodes:
//!
//! - [`kafka_sub`] — producer that consumes messages from a Kafka topic and emits them as events
//! - [`kafka_pub`] — consumer that produces messages to a Kafka topic
//!
//! # Setup
//!
//! ## Local (Docker)
//!
//! Using Redpanda (Kafka-compatible, faster startup):
//!
//! ```sh
//! docker run --rm -p 9092:9092 \
//!   -e REDPANDA_ADVERTISE_KAFKA_ADDRESS=localhost:9092 \
//!   docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
//!   redpanda start --overprovisioned --smp 1 --memory 512M \
//!   --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
//! ```
//!
//! # Subscribing to a topic
//!
//! [`kafka_sub`] consumes messages from a Kafka topic, starting from the configured offset.
//! Each message becomes a [`KafkaEvent`] containing the key, value, topic, partition, and offset.
//!
//! ```ignore
//! use wingfoil::adapters::kafka::*;
//! use wingfoil::*;
//!
//! let conn = KafkaConnection::new("localhost:9092");
//!
//! kafka_sub(conn, "my-topic", "my-group")
//!     .collapse()
//!     .for_each(|event, _| {
//!         println!("{}: {:?}", event.topic, event.value_str())
//!     })
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Publishing messages
//!
//! [`kafka_pub`] (or the fluent `.kafka_pub()` method) consumes a
//! `Burst<KafkaRecord>` stream and produces each record to Kafka.
//!
//! ```ignore
//! use wingfoil::adapters::kafka::*;
//! use wingfoil::*;
//!
//! let conn = KafkaConnection::new("localhost:9092");
//!
//! constant(burst![
//!     KafkaRecord { topic: "my-topic".into(), key: Some(b"key1".to_vec()), value: b"hello".to_vec() },
//! ])
//! .kafka_pub(conn)
//! .run(RunMode::RealTime, RunFor::Cycles(1))
//! .unwrap();
//! ```
//!
//! # Round-trip example
//!
//! See [`examples/kafka`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kafka)
//! for a full working example that produces messages, consumes them, transforms
//! the values, and writes the results to a second topic.
//!
//! ```ignore
//! use wingfoil::adapters::kafka::*;
//! use wingfoil::*;
//!
//! let conn = KafkaConnection::new("localhost:9092");
//!
//! let seed = constant(burst![
//!     KafkaRecord { topic: "source".into(), key: Some(b"greeting".to_vec()), value: b"hello".to_vec() },
//!     KafkaRecord { topic: "source".into(), key: Some(b"subject".to_vec()), value: b"world".to_vec() },
//! ])
//! .kafka_pub(conn.clone());
//!
//! let round_trip = kafka_sub(conn.clone(), "source", "example-group")
//!     .map(|burst| {
//!         burst.into_iter().map(|event| {
//!             let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
//!             KafkaRecord { topic: "dest".into(), key: event.key, value: upper }
//!         })
//!         .collect::<Burst<KafkaRecord>>()
//!     })
//!     .kafka_pub(conn);
//!
//! Graph::new(vec![seed, round_trip], RunMode::RealTime, RunFor::Cycles(3)).run().unwrap();
//! ```

mod read;
mod write;

#[cfg(all(test, feature = "kafka-integration-test"))]
mod integration_tests;

pub use read::*;
pub use write::*;

/// Connection configuration for Kafka.
#[derive(Debug, Clone)]
pub struct KafkaConnection {
    /// Kafka bootstrap servers, e.g. `"localhost:9092"`.
    pub brokers: String,
}

impl KafkaConnection {
    /// Create a connection config with a broker string.
    ///
    /// Multiple brokers can be separated by commas: `"host1:9092,host2:9092"`.
    pub fn new(brokers: impl Into<String>) -> Self {
        Self {
            brokers: brokers.into(),
        }
    }
}

/// A record to be produced to Kafka.
#[derive(Debug, Clone, Default)]
pub struct KafkaRecord {
    /// The target topic.
    pub topic: String,
    /// Optional message key (used for partitioning).
    pub key: Option<Vec<u8>>,
    /// The message payload.
    pub value: Vec<u8>,
}

impl KafkaRecord {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }
}

/// An event consumed from a Kafka topic.
#[derive(Debug, Clone, Default)]
pub struct KafkaEvent {
    /// The source topic.
    pub topic: String,
    /// The partition from which this message was consumed.
    pub partition: i32,
    /// The offset of this message within the partition.
    pub offset: i64,
    /// The message key, if present.
    pub key: Option<Vec<u8>>,
    /// The message payload.
    pub value: Vec<u8>,
}

impl KafkaEvent {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }

    /// Interpret the key as a UTF-8 string, if present.
    pub fn key_str(&self) -> Option<Result<&str, std::str::Utf8Error>> {
        self.key.as_deref().map(std::str::from_utf8)
    }
}
