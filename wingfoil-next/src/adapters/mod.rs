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

pub mod lines;
