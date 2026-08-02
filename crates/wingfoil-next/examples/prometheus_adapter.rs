//! prometheus adapter example — serve `GET /metrics` so Grafana can scrape a
//! wingfoil stream value.
//!
//! A port of the classic `wingfoil/examples/telemetry/prometheus` example onto
//! the next engine. The exporter serves the Prometheus text format on port 9091;
//! point Grafana's Prometheus data source at it (or scrape it directly).
//!
//! # Run
//!
//! ```sh
//! cargo run -p wingfoil-next --example prometheus_adapter --features prometheus
//! ```
//!
//! Then scrape it:
//!
//! ```sh
//! curl http://localhost:9091/metrics
//! # TYPE wingfoil_ticks_total gauge
//! wingfoil_ticks_total 5
//! ```
//!
//! Press Ctrl+C to stop.

use std::time::Duration;

use wingfoil_next::adapters::prometheus::{PrometheusExporter, PrometheusSinkOps};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    // Bind the exporter synchronously so a bind error surfaces before the run.
    let exporter = PrometheusExporter::new("0.0.0.0:9091");
    let port = exporter.serve()?;
    println!("Prometheus metrics available at http://localhost:{port}/metrics");

    let g = GraphBuilder::new();
    // The gauge sink is wired into `g` at this call; the returned handle is the
    // graph's only root. Run it forever (Ctrl+C stops).
    let _metric = g
        .ticker(Duration::from_secs(1))
        .count()
        .prometheus_gauge(&exporter, "wingfoil_ticks_total");

    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}
