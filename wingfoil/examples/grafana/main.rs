#![doc = include_str!("./README.md")]

use std::time::Duration;
use wingfoil::adapters::grafana::PrometheusExporter;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // ── Prometheus exporter ────────────────────────────────────────────────
    let exporter = PrometheusExporter::new("0.0.0.0:9091");
    let port = exporter.serve()?;
    println!("Prometheus metrics available at http://localhost:{port}/metrics");

    let counter = ticker(Duration::from_secs(1)).count();
    let metric_node = exporter.register("wingfoil_ticks_total", counter.clone());

    // ── Run ────────────────────────────────────────────────────────────────
    // For OTLP push support, see the `otlp_metrics` example.
    Graph::new(vec![metric_node], RunMode::RealTime, RunFor::Forever).run()?;
    Ok(())
}
