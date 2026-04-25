//! OpenTelemetry OTLP adapter: metrics and trace spans.
//!
//! Exports stream values as OTLP gauge metrics and trace spans to any
//! OTLP-compatible backend (Grafana Alloy, Datadog, Honeycomb, New Relic, etc.).
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0
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
pub mod traces;
pub use push::{OtlpConfig, OtlpPush};
pub use traces::OtlpSpans;

/// Re-exported so callers of [`OtlpSpans::otlp_spans`] can construct
/// attribute vectors without adding `opentelemetry` to their own
/// dependencies.
pub use opentelemetry::KeyValue;

#[cfg(all(test, feature = "otlp-integration-test"))]
mod integration_tests;
