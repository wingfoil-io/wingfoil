//! Fluvio adapter — streaming reads (consumer) and writes (producer) for Fluvio topics.
//!
//! Provides two graph nodes:
//!
//! - [`fluvio_sub`] — producer that streams records from a Fluvio topic partition
//! - [`fluvio_pub`] — consumer that writes records to a Fluvio topic
//!
//! # Setup
//!
//! ## Local (Docker)
//!
//! Start a single-node Fluvio cluster:
//!
//! ```sh
//! docker run --rm -d -p 9003:9003 -p 9010:9010 \
//!   --name fluvio-sc \
//!   infinyon/fluvio:0.18.1 \
//!   fluvio-run sc --local /tmp/fluvio
//! ```
//!
//! Create a topic before producing or consuming:
//!
//! ```sh
//! fluvio topic create my-topic
//! ```
//!
//! # Subscribing to a topic
//!
//! [`fluvio_sub`] streams records from a Fluvio topic partition starting at the
//! specified offset (or from the beginning if `start_offset` is `None`).
//!
//! ```ignore
//! use wingfoil::adapters::fluvio::*;
//! use wingfoil::*;
//!
//! let conn = FluvioConnection::new("127.0.0.1:9003");
//!
//! fluvio_sub(conn, "my-topic", 0, None)
//!     .collapse()
//!     .for_each(|event, _| {
//!         println!("offset={} value={:?}", event.offset, event.value_str())
//!     })
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Publishing records to a topic
//!
//! [`fluvio_pub`] (or the fluent `.fluvio_pub()` method) consumes a
//! `Burst<FluvioRecord>` stream and writes each record to a Fluvio topic.
//!
//! ```ignore
//! use wingfoil::adapters::fluvio::*;
//! use wingfoil::*;
//!
//! let conn = FluvioConnection::new("127.0.0.1:9003");
//!
//! constant(burst![
//!     FluvioRecord::with_key("greeting", b"hello".to_vec()),
//!     FluvioRecord::new(b"world".to_vec()),
//! ])
//! .fluvio_pub(conn, "my-topic")
//! .run(RunMode::RealTime, RunFor::Cycles(1))
//! .unwrap();
//! ```
//!
//! # Round-trip example
//!
//! See [`examples/fluvio`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/fluvio)
//! for a full working example that seeds records with `fluvio_pub`, reads them back
//! with `fluvio_sub`, and transforms the values.

mod read;
mod write;

#[cfg(all(test, feature = "fluvio-integration-test"))]
mod integration_tests;

pub use read::*;
pub use write::*;

/// Connection configuration for a Fluvio cluster.
#[derive(Debug, Clone)]
pub struct FluvioConnection {
    /// Fluvio SC (System Controller) endpoint in `host:port` format.
    ///
    /// The default Fluvio SC port is 9003. Do **not** include a URL scheme
    /// (`http://` or `https://`) — Fluvio uses a custom binary protocol.
    ///
    /// Example: `"127.0.0.1:9003"`
    pub endpoint: String,
}

impl FluvioConnection {
    /// Create a connection config with the given SC endpoint.
    ///
    /// The endpoint should be in `host:port` format, e.g. `"127.0.0.1:9003"`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

/// A record to write to a Fluvio topic.
#[derive(Debug, Clone, Default)]
pub struct FluvioRecord {
    /// Optional record key. `None` sends the record without a key (`RecordKey::NULL`).
    pub key: Option<String>,
    /// Record payload as raw bytes.
    pub value: Vec<u8>,
}

impl FluvioRecord {
    /// Create a keyless record with the given value.
    pub fn new(value: Vec<u8>) -> Self {
        Self { key: None, value }
    }

    /// Create a record with both key and value.
    pub fn with_key(key: impl Into<String>, value: Vec<u8>) -> Self {
        Self {
            key: Some(key.into()),
            value,
        }
    }
}

/// A record received from a Fluvio topic.
#[derive(Debug, Clone, Default)]
pub struct FluvioEvent {
    /// Record key, if present. `None` for keyless records.
    pub key: Option<Vec<u8>>,
    /// Record payload as raw bytes.
    pub value: Vec<u8>,
    /// Absolute partition offset of this record.
    pub offset: i64,
}

impl FluvioEvent {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }

    /// Interpret the key as a UTF-8 string, if present.
    pub fn key_str(&self) -> Option<Result<&str, std::str::Utf8Error>> {
        self.key.as_deref().map(std::str::from_utf8)
    }
}
