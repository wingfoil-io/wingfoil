//! Prometheus I/O adapter for real-time metrics visualization.
//!
//! Serves `GET /metrics` in Prometheus text format so Grafana can scrape it
//! via its Prometheus data source:
//!
//! ```no_run
//! use wingfoil::adapters::prometheus::PrometheusExporter;
//! use wingfoil::*;
//! use std::time::Duration;
//!
//! let exporter = PrometheusExporter::new("0.0.0.0:9091");
//! let port = exporter.serve().expect("failed to bind metrics server");
//!
//! let counter = ticker(Duration::from_secs(1)).count();
//! let node = exporter.register("wingfoil_counter_total", counter.clone());
//!
//! node.run(RunMode::RealTime, RunFor::Forever).unwrap();
//! ```
//!
//! For push-based metrics export see the `otlp` adapter.

pub mod exporter;

pub use exporter::PrometheusExporter;

#[cfg(all(test, feature = "prometheus-integration-test"))]
mod integration_tests;
