//! Grafana I/O adapter for real-time metrics visualization.
//!
//! Serves `GET /metrics` in Prometheus text format so Grafana can scrape it
//! via its Prometheus data source:
//!
//! ```no_run
//! use wingfoil::adapters::grafana::PrometheusExporter;
//! use wingfoil::*;
//! use std::time::Duration;
//!
//! let exporter = PrometheusExporter::new("0.0.0.0:9091");
//! exporter.serve(); // spawns HTTP server thread
//!
//! let counter = ticker(Duration::from_secs(1)).count();
//! let node = exporter.register("wingfoil_counter_total", counter.clone());
//!
//! node.run(RunMode::RealTime, RunFor::Forever).unwrap();
//! ```
//!
//! For push-based metrics export see the `otlp` adapter.

pub mod prometheus;

pub use prometheus::PrometheusExporter;

#[cfg(all(test, feature = "grafana-integration-test"))]
mod integration_tests;
