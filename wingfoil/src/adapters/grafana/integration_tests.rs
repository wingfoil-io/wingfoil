//! Integration tests for the Grafana adapter.
//!
//! Requires the Docker stack from `docker/grafana/`:
//! ```sh
//! cd docker/grafana && docker compose up -d
//! ```
//!
//! Then create a service account token (see `docker/grafana/README.md`) and run:
//! ```sh
//! GRAFANA_TEST_URL=http://localhost:3000 \
//! GRAFANA_TEST_API_KEY=<token> \
//! RUST_LOG=INFO cargo test --features grafana-integration-test -p wingfoil -- --test-threads=1 --nocapture
//! ```

use super::*;
use crate::{Graph, RunFor, RunMode, nodes::*};
use std::time::Duration;

fn grafana_url() -> String {
    std::env::var("GRAFANA_TEST_URL").unwrap_or_else(|_| "http://localhost:3000".into())
}

fn grafana_api_key() -> String {
    std::env::var("GRAFANA_TEST_API_KEY")
        .expect("GRAFANA_TEST_API_KEY must be set for grafana integration tests")
}

fn grafana_org_id() -> u64 {
    std::env::var("GRAFANA_TEST_ORG_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

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
    exporter.serve();

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

// ─── Grafana Live push ───────────────────────────────────────────────────────

#[test]
fn grafana_live_push_accepts_data() {
    _ = env_logger::try_init();

    let config = GrafanaConfig {
        url: grafana_url(),
        api_key: grafana_api_key(),
        org_id: grafana_org_id(),
    };

    let counter = ticker(Duration::from_millis(100)).count();
    let node = counter.grafana_push("stream/wingfoil/integration_test", config);

    // Push for 1 second — we just verify no errors are returned
    node.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(1)))
        .unwrap();
}

#[test]
fn grafana_live_push_wrong_key_returns_error() {
    _ = env_logger::try_init();

    let config = GrafanaConfig {
        url: grafana_url(),
        api_key: "invalid-key".into(),
        org_id: 1,
    };

    let counter = ticker(Duration::from_millis(50)).count();
    let node = counter.grafana_push("stream/wingfoil/auth_test", config);

    let result = node.run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(result.is_err(), "expected auth failure, got Ok");
    let err = format!("{:?}", result.unwrap_err());
    assert!(
        err.contains("401") || err.contains("403") || err.contains("grafana_push"),
        "unexpected error message: {err}"
    );
}

// ─── Combined: exporter + push in same graph ────────────────────────────────

#[test]
fn exporter_and_push_together() {
    _ = env_logger::try_init();
    let port = metrics_port() + 1; // avoid conflict with the scrape test

    let exporter = PrometheusExporter::new(format!("0.0.0.0:{port}"));
    exporter.serve();

    let config = GrafanaConfig {
        url: grafana_url(),
        api_key: grafana_api_key(),
        org_id: grafana_org_id(),
    };

    let counter = ticker(Duration::from_millis(200)).count();
    let metric_node = exporter.register("wingfoil_combined_counter", counter.clone());
    let push_node = counter.grafana_push("stream/wingfoil/combined", config);

    Graph::new(
        vec![metric_node, push_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(1)),
    )
    .run()
    .unwrap();
}
