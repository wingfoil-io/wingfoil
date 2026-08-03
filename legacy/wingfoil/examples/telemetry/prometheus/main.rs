#![doc = include_str!("./README.md")]

use std::time::Duration;
use wingfoil::adapters::prometheus::PrometheusExporter;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // ── Prometheus exporter ────────────────────────────────────────────────
    let exporter = PrometheusExporter::new("0.0.0.0:9091");
    let port = exporter.serve()?;
    println!("Prometheus metrics available at http://localhost:{port}/metrics");

    let counter = ticker(Duration::from_secs(1)).count();

    // ── Run ────────────────────────────────────────────────────────────────
    // For OTLP push support, see the `otlp_metrics` example.
    exporter
        .register("wingfoil_ticks_total", counter)
        .graph()
        .real_time()
        .forever()
        .run()?;
    Ok(())
}
