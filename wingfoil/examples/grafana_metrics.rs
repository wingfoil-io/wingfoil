//! Demonstrates the Grafana adapter: Prometheus exporter + Grafana Live push.
//!
//! # Prometheus exporter
//!
//! Serves `GET /metrics` on port 9091. Point Grafana's Prometheus data source
//! at `http://<host>:9091` (or use the Docker stack in `docker/grafana/`).
//!
//! # Grafana Live push
//!
//! Pushes values to a Grafana Live channel. Requires a running Grafana instance
//! and a service account token:
//!
//! ```sh
//! GRAFANA_API_KEY=<token> cargo run --example grafana_metrics --features grafana
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::adapters::grafana::{GrafanaConfig, GrafanaPush, PrometheusExporter};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // ── Prometheus exporter ────────────────────────────────────────────────
    let exporter = PrometheusExporter::new("0.0.0.0:9091");
    exporter.serve();
    println!("Prometheus metrics available at http://localhost:9091/metrics");

    let counter = ticker(Duration::from_secs(1)).count();
    let metric_node = exporter.register("wingfoil_ticks_total", counter.clone());

    // ── Grafana Live push (optional — requires GRAFANA_API_KEY) ───────────
    let push_node: Option<Rc<dyn Node>> = std::env::var("GRAFANA_API_KEY").ok().map(|api_key| {
        let config = GrafanaConfig {
            url: std::env::var("GRAFANA_URL").unwrap_or_else(|_| "http://localhost:3000".into()),
            api_key,
            org_id: 1,
        };
        println!("Pushing to Grafana Live: stream/wingfoil/ticks");
        counter.grafana_push("stream/wingfoil/ticks", config)
    });

    // ── Run ────────────────────────────────────────────────────────────────
    let mut nodes: Vec<Rc<dyn Node>> = vec![metric_node];
    if let Some(node) = push_node {
        nodes.push(node);
    }

    Graph::new(nodes, RunMode::RealTime, RunFor::Forever).run()?;
    Ok(())
}
