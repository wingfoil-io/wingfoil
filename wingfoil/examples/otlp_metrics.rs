//! Demonstrates pushing wingfoil metrics via OpenTelemetry OTLP.
//!
//! Runs a Prometheus exporter (pull) alongside an OTLP push sink so metrics
//! can be consumed by both Grafana/Prometheus scrapers and any OTLP-compatible
//! backend (Grafana Alloy, Datadog, Honeycomb, New Relic, etc.).
//!
//! # Usage
//!
//! ```sh
//! # Start an OTel collector (e.g. via docker/grafana stack or standalone)
//! docker run --rm -p 4318:4318 otel/opentelemetry-collector:latest
//!
//! # Run this example
//! OTLP_ENDPOINT=http://localhost:4318 \
//!     cargo run --example otlp_metrics --features otlp,prometheus
//! ```
use std::time::Duration;
use wingfoil::adapters::otlp::{OtlpConfig, OtlpPush};
use wingfoil::adapters::prometheus::PrometheusExporter;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // ── Prometheus exporter (pull) ─────────────────────────────────────────
    let exporter = PrometheusExporter::new("0.0.0.0:9091");
    let port = exporter.serve()?;
    println!("Prometheus metrics at http://localhost:{port}/metrics");

    // ── OTLP push ─────────────────────────────────────────────────────────
    let endpoint =
        std::env::var("OTLP_ENDPOINT").unwrap_or_else(|_| "http://localhost:4318".into());
    let config = OtlpConfig {
        endpoint: endpoint.clone(),
        service_name: "wingfoil-example".into(),
    };

    let counter = ticker(Duration::from_secs(1)).count();
    let metric_node = exporter.register("wingfoil_ticks_total", counter.clone());
    let otlp_node = counter.otlp_push("wingfoil_ticks_total", config);

    println!("Pushing OTLP metrics to {endpoint}");

    Graph::new(
        vec![metric_node, otlp_node],
        RunMode::RealTime,
        RunFor::Forever,
    )
    .run()?;
    Ok(())
}
