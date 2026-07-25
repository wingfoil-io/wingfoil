//! I/O adapters — the graph's edges to the outside world, built strictly on
//! the public Op-pattern API (sources over
//! [`channel`](crate::fluent::SourceOps::channel) / [`poll`](crate::fluent::SourceOps::poll),
//! sinks over [`for_each`](crate::fluent::StreamOps::for_each)).
//!
//! Each adapter lives in its own module and stays *out* of the
//! [`prelude`](crate::prelude); bring one in explicitly, e.g.
//! `use wingfoil_next::adapters::lines::LinesSinkOps;`. This mirrors the
//! [`stats`](crate::stats) module's extension-trait layering.
//!
//! - [`lines`] — a dependency-free, line-oriented file adapter (historical
//!   replay source + realtime tail + file sink), the smallest complete
//!   demonstration of an I/O edge in both directions.
//! - [`csv`] — a serde-typed CSV file adapter (historical replay source + file
//!   sink) behind the `csv` feature, the parsing cousin of [`lines`].
//! - [`augurs`] — on-graph time-series analysis (forecasting + outlier
//!   detection) over sliding windows, behind the `augurs` feature. A pure-Rust
//!   compute adapter (no service), so it is transform ops, not a source/sink.
//! - [`etcd`] — a streaming key-prefix snapshot + watch source (`etcd_sub`) and
//!   a key-value PUT sink (`EtcdSinkOps::etcd_pub`) for the etcd key-value
//!   store, behind the `etcd` feature (built on the async `produce_async`
//!   ergonomic).
//! - [`cache`] — a file-backed, query-keyed, LRU-evicting result cache for
//!   time-sliced historical readers, behind the `cache` feature. A pure utility
//!   (async `get`/`put`), not a source/sink op.
//! - [`common`] — helpers shared across adapters (the out-of-window row
//!   [`WindowFilter`](common::WindowFilter) for caller-parameterised historical
//!   replays). Always compiled, dependency-light.
//! - [`prometheus`] — a realtime, pull-based metrics sink serving a
//!   `GET /metrics` endpoint in Prometheus text format (register streams as
//!   gauges via `PrometheusSinkOps::prometheus_gauge`), behind the `prometheus`
//!   feature. A sink only; a no-op under historical replay.
//! - [`redis`] — Redis Pub/Sub (`redis_sub` source + `RedisSinkOps::redis_pub`
//!   sink) and Streams (`redis_stream_read` source +
//!   `RedisStreamSinkOps::redis_stream_write` sink), behind the `redis` feature
//!   (built on the async `produce_async` / `consume_async` ergonomics). The
//!   sources are realtime-only.
//! - [`zmq`] — real-time ØMQ pub/sub (`zmq_sub` source with a connection-status
//!   stream + `ZeroMqPub::zmq_pub` sink), with optional etcd service discovery,
//!   behind the `zmq` feature. Synchronous/poll-based, so it uses a background
//!   thread over the `channel` layer (not `async`); the source is realtime-only.

#[cfg(feature = "augurs")]
pub mod augurs;
#[cfg(feature = "cache")]
pub mod cache;
pub mod common;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
pub mod lines;
#[cfg(feature = "prometheus")]
pub mod prometheus;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(feature = "zmq")]
pub mod zmq;
