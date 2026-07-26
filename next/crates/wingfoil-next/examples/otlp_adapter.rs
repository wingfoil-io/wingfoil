//! otlp adapter example — push a wingfoil stream value to an OpenTelemetry
//! backend via OTLP, alongside a Prometheus `/metrics` endpoint for pull-based
//! scraping.
//!
//! A port of the classic `wingfoil/examples/telemetry/otlp` example onto the
//! next engine. Runs a Prometheus exporter (pull) and an OTLP push sink over the
//! same counter, so metrics can be consumed by both Prometheus scrapers and any
//! OTLP-compatible backend (Grafana Alloy, Datadog, Honeycomb, New Relic, …).
//!
//! The graph owns the tokio runtime `otlp_push` exports on — created lazily, so
//! no `&Handle` is threaded in (see the adapter module docs). The graph is driven
//! from this (non-async) `main`, as `consume_async` requires.
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0
//! ```
//!
//! # Run
//!
//! ```sh
//! OTLP_ENDPOINT=http://localhost:4318 \
//!     cargo run -p wingfoil-next --example otlp_adapter --features otlp,prometheus
//! ```
//!
//! Press Ctrl+C to stop.

use std::time::Duration;

use wingfoil::{RunFor, RunMode};
use wingfoil_next::adapters::otlp::{OtlpConfig, OtlpSinkOps};
use wingfoil_next::adapters::prometheus::{PrometheusExporter, PrometheusSinkOps};
use wingfoil_next::prelude::*;

fn main() -> anyhow::Result<()> {
    // ── Prometheus exporter (pull) ─────────────────────────────────────────
    let exporter = PrometheusExporter::new("0.0.0.0:9091");
    let port = exporter.serve()?;
    println!("Prometheus metrics at http://localhost:{port}/metrics");

    // ── OTLP push ──────────────────────────────────────────────────────────
    let endpoint =
        std::env::var("OTLP_ENDPOINT").unwrap_or_else(|_| "http://localhost:4318".into());
    let config = OtlpConfig::new(endpoint.clone(), "wingfoil-next-example");

    let g = GraphBuilder::new();
    let counter = g.ticker(Duration::from_secs(1)).count();
    let _prometheus = counter.prometheus_gauge(&exporter, "wingfoil_ticks_total");
    let _otlp = counter.otlp_push("wingfoil_ticks_total", config)?;

    println!("Pushing OTLP metrics to {endpoint}");
    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}
