//! Grafana I/O adapter for real-time metrics visualization.
//!
//! Two integration points:
//!
//! ## 1. Prometheus exporter
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
//! ## 2. Grafana Live push
//!
//! Pushes stream values directly to a Grafana Live channel:
//!
//! ```no_run
//! use wingfoil::adapters::grafana::{GrafanaConfig, GrafanaPush};
//! use wingfoil::*;
//! use std::time::Duration;
//!
//! let config = GrafanaConfig {
//!     url: "http://localhost:3000".into(),
//!     api_key: std::env::var("GRAFANA_API_KEY").unwrap(),
//!     org_id: 1,
//! };
//!
//! ticker(Duration::from_millis(100))
//!     .count()
//!     .grafana_push("stream/wingfoil/counter", config)
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```

pub mod live;
pub mod prometheus;

pub use live::{GrafanaConfig, GrafanaPush};
pub use prometheus::PrometheusExporter;

#[cfg(all(test, feature = "grafana-integration-test"))]
mod integration_tests;
