//! OpenTelemetry OTLP metrics adapter.
//!
//! Pushes stream values as gauge metrics to any OTLP-compatible backend
//! (Grafana Alloy, Datadog, Honeycomb, New Relic, etc.).
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 4318:4318 otel/opentelemetry-collector:latest
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use wingfoil::adapters::otlp::{OtlpConfig, OtlpPush};
//!
//! let config = OtlpConfig {
//!     endpoint: "http://localhost:4318".into(),
//!     service_name: "my-app".into(),
//! };
//! counter.otlp_push("wingfoil_ticks_total", config);
//! ```

pub mod push;
pub use push::{OtlpConfig, OtlpPush};

#[cfg(all(test, feature = "otlp-integration-test"))]
mod integration_tests;
