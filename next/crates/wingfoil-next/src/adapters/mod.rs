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

#[cfg(feature = "augurs")]
pub mod augurs;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
pub mod lines;
