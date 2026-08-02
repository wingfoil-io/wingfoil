//! Redis adapter — streaming Pub/Sub reads (`SUBSCRIBE`) and writes (`PUBLISH`).
//!
//! Provides two transports, each with a producer and a consumer:
//!
//! **Pub/Sub** (fire-and-forget channels):
//! - [`redis_sub`] — producer that subscribes to a channel and emits each message as an event
//! - [`redis_pub`] — consumer that publishes messages to Redis channels
//!
//! **Streams** (a persistent, replayable log):
//! - [`redis_stream_read`] — producer that emits a snapshot of existing entries
//!   followed by live entries as they are appended
//! - [`redis_stream_write`] — consumer that appends entries to a stream via `XADD`
//!
//! Redis Pub/Sub is **fire-and-forget**: messages are delivered only to clients
//! subscribed at the moment of publication. There is no backlog, no replay, and no
//! offsets — a subscriber sees only what is published after its `SUBSCRIBE` completes.
//! Redis Streams, by contrast, persist entries with monotonic IDs, so
//! [`redis_stream_read`] can replay history before tailing live appends.
//!
//! # Setup
//!
//! ## Local (Docker)
//!
//! ```sh
//! docker run --rm -p 6379:6379 redis:7-alpine
//! ```
//!
//! # Subscribing to a channel
//!
//! [`redis_sub`] subscribes to a single channel and streams every message published to it
//! as a [`RedisEvent`].
//!
//! ```ignore
//! use wingfoil::adapters::redis::*;
//! use wingfoil::*;
//!
//! let conn = RedisConnection::new("redis://127.0.0.1:6379");
//!
//! redis_sub(conn, "prices")
//!     .collapse()
//!     .for_each(|event, _| {
//!         println!("{}: {:?}", event.channel, event.payload_str())
//!     })
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Publishing messages
//!
//! [`redis_pub`] (or the fluent `.redis_pub()` method) consumes a `Burst<RedisEntry>`
//! stream and publishes each entry to the channel it names.
//!
//! ```ignore
//! use wingfoil::adapters::redis::*;
//! use wingfoil::*;
//!
//! let conn = RedisConnection::new("redis://127.0.0.1:6379");
//!
//! constant(burst![
//!     RedisEntry { channel: "prices".into(), payload: b"42".to_vec() },
//! ])
//! .redis_pub(conn)
//! .run(RunMode::RealTime, RunFor::Cycles(1))
//! .unwrap();
//! ```
//!
//! # Reading and writing streams
//!
//! [`redis_stream_read`] first emits all existing entries under the stream key
//! (via `XRANGE`), capturing the last entry ID, then tails live appends (via
//! `XREAD BLOCK` from that ID) so no entry is missed in the handoff.
//!
//! ```ignore
//! use wingfoil::adapters::redis::*;
//! use wingfoil::*;
//!
//! let conn = RedisConnection::new("redis://127.0.0.1:6379");
//!
//! // Append an entry.
//! constant(burst![RedisStreamRecord::single("events", "kind", b"login".to_vec())])
//!     .redis_stream_write(conn.clone())
//!     .run(RunMode::RealTime, RunFor::Cycles(1))
//!     .unwrap();
//!
//! // Replay history, then tail live entries.
//! redis_stream_read(conn, "events")
//!     .collapse()
//!     .for_each(|event, _| println!("{} {:?}", event.id, event.fields))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Round-trip example
//!
//! See [`examples/redis`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/redis)
//! for a full working example that publishes messages, subscribes to them, transforms
//! the payloads, and republishes the results to a second channel.

mod read;
mod stream;
mod write;

#[cfg(all(test, feature = "redis-integration-test"))]
mod integration_tests;

pub use read::*;
pub use stream::*;
pub use write::*;

/// Connection configuration for Redis.
#[derive(Debug, Clone)]
pub struct RedisConnection {
    /// Redis connection URL, e.g. `"redis://127.0.0.1:6379"`.
    ///
    /// Supports the standard `redis://`/`rediss://` URL scheme, including an
    /// optional password and database index: `"redis://:pass@host:6379/0"`.
    pub url: String,
}

impl RedisConnection {
    /// Create a connection config from a Redis URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

impl From<&str> for RedisConnection {
    fn from(url: &str) -> Self {
        Self::new(url)
    }
}

impl From<String> for RedisConnection {
    fn from(url: String) -> Self {
        Self::new(url)
    }
}

/// A message to publish to a Redis channel.
#[derive(Debug, Clone, Default)]
pub struct RedisEntry {
    /// The target channel.
    pub channel: String,
    /// The message payload.
    pub payload: Vec<u8>,
}

impl RedisEntry {
    /// Interpret the payload as a UTF-8 string.
    pub fn payload_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.payload)
    }
}

/// A message received from a subscribed Redis channel.
#[derive(Debug, Clone, Default)]
pub struct RedisEvent {
    /// The channel the message was published to.
    pub channel: String,
    /// The message payload.
    pub payload: Vec<u8>,
}

impl RedisEvent {
    /// Interpret the payload as a UTF-8 string.
    pub fn payload_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.payload)
    }
}

/// A record to append to a Redis stream via `XADD`.
///
/// A Redis stream entry is an ordered map of field → value pairs. The entry ID is
/// assigned by Redis (`XADD key *`), so it is not part of the record.
#[derive(Debug, Clone, Default)]
pub struct RedisStreamRecord {
    /// The target stream key.
    pub key: String,
    /// The entry's field → value pairs, in insertion order.
    pub fields: Vec<(String, Vec<u8>)>,
}

impl RedisStreamRecord {
    /// Build a single-field record (`field` → `value`) for stream `key`.
    pub fn single(
        key: impl Into<String>,
        field: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            key: key.into(),
            fields: vec![(field.into(), value.into())],
        }
    }
}

/// An entry read from a Redis stream via `XRANGE` (snapshot) or `XREAD` (tail).
#[derive(Debug, Clone, Default)]
pub struct RedisStreamEvent {
    /// The source stream key.
    pub key: String,
    /// The Redis-assigned entry ID, e.g. `"1526919030474-0"`.
    pub id: String,
    /// The entry's field → value pairs.
    pub fields: Vec<(String, Vec<u8>)>,
}

impl RedisStreamEvent {
    /// Look up a field's value by name.
    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_slice())
    }
}
