//! Integration tests for the Grafana adapter.
//!
//! Requires the Docker stack from `docker/grafana/`:
//! ```sh
//! docker compose -f docker/grafana/docker-compose.yml up -d
//! ```
//!
//! Then run:
//! ```sh
//! RUST_LOG=INFO cargo test --features grafana-integration-test -p wingfoil -- --test-threads=1 --nocapture
//! ```

use super::*;
use crate::{RunFor, RunMode, nodes::*};
use std::time::Duration;

fn prometheus_url() -> String {
    std::env::var("PROMETHEUS_TEST_URL").unwrap_or_else(|_| "http://localhost:9090".into())
}

fn metrics_port() -> u16 {
    std::env::var("WINGFOIL_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9091)
}

// ─── Prometheus exporter ────────────────────────────────────────────────────

#[test]
fn prometheus_exporter_scrapeable_by_prometheus() {
    _ = env_logger::try_init();
    let port = metrics_port();
    let exporter = PrometheusExporter::new(format!("0.0.0.0:{port}"));
    exporter.serve().unwrap();

    let counter = ticker(Duration::from_millis(100)).count();
    let node = exporter.register("wingfoil_integration_counter", counter);

    // Run long enough for Prometheus to complete at least one scrape (interval = 5s)
    node.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(7)))
        .unwrap();

    // Poll Prometheus until the metric appears (scrape may lag by up to one interval)
    let prom = prometheus_url();
    let mut found = false;
    for attempt in 1..=12 {
        let resp = reqwest::blocking::get(format!(
            "{prom}/api/v1/query?query=wingfoil_integration_counter"
        ))
        .expect("Prometheus query failed");

        assert!(
            resp.status().is_success(),
            "Prometheus query returned {}",
            resp.status()
        );

        let body: serde_json::Value = resp.json().expect("Prometheus response was not JSON");
        let result = &body["data"]["result"];
        if !result.as_array().unwrap_or(&vec![]).is_empty() {
            log::info!("Prometheus scrape result (attempt {attempt}): {result}");
            found = true;
            break;
        }
        log::info!("attempt {attempt}: metric not yet in Prometheus, retrying...");
        std::thread::sleep(Duration::from_secs(5));
    }
    assert!(
        found,
        "wingfoil_integration_counter never appeared in Prometheus after polling"
    );
}
