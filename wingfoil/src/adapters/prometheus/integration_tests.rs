//! Integration tests for the Prometheus adapter.
//!
//! Requires the Docker stack from `docker/grafana/`:
//! ```sh
//! docker compose -f docker/grafana/docker-compose.yml up -d
//! ```
//!
//! Then run:
//! ```sh
//! RUST_LOG=INFO cargo test --features prometheus-integration-test -p wingfoil -- --test-threads=1 --nocapture
//! ```

use super::*;
use crate::{Graph, NanoTime, RunFor, RunMode, nodes::*};
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

fn get_raw_metrics(port: u16) -> String {
    let resp = reqwest::blocking::get(format!("http://localhost:{port}/metrics"))
        .expect("metrics request failed");
    resp.text().expect("metrics response not text")
}

fn prometheus_available() -> bool {
    reqwest::blocking::get(format!("{}/api/v1/query?query=up", prometheus_url()))
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ─── Self-contained tests (no running Prometheus required) ──────────────────

#[test]
fn test_connection_refused() {
    // Occupy a port so the exporter cannot bind it.
    let occupied =
        std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind test listener");
    let port = occupied.local_addr().unwrap().port();

    let exporter = PrometheusExporter::new(format!("127.0.0.1:{port}"));
    let result = exporter.serve();
    assert!(result.is_err(), "expected bind error when port is occupied");
}

#[test]
fn historical_mode_produces_no_scraped_metrics() {
    _ = env_logger::try_init();
    // Port offset to avoid clashing with other tests
    let exporter = PrometheusExporter::new("127.0.0.1:0");
    let port = exporter.serve().unwrap();

    let counter = ticker(Duration::from_millis(10)).count();
    let node = exporter.register("hist_integration_counter", counter);

    node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
        .unwrap();

    let body = get_raw_metrics(port);
    assert!(
        body.is_empty(),
        "expected empty body in historical mode, got:\n{body}"
    );
}

#[test]
fn multiple_metrics_all_appear_in_scrape() {
    _ = env_logger::try_init();
    let exporter = PrometheusExporter::new("127.0.0.1:0");
    let port = exporter.serve().unwrap();

    let counter = ticker(Duration::from_millis(10)).count();
    let doubled = counter.clone().map(|n| n * 2);

    let node_a = exporter.register("multi_counter_a", counter);
    let node_b = exporter.register("multi_counter_b", doubled);

    Graph::new(vec![node_a, node_b], RunMode::RealTime, RunFor::Cycles(3))
        .run()
        .unwrap();

    let body = get_raw_metrics(port);
    assert!(
        body.contains("multi_counter_a"),
        "missing multi_counter_a in:\n{body}"
    );
    assert!(
        body.contains("multi_counter_b"),
        "missing multi_counter_b in:\n{body}"
    );
}

// ─── Prometheus scrape test (requires Docker stack) ─────────────────────────

#[test]
fn prometheus_exporter_scrapeable_by_prometheus() {
    _ = env_logger::try_init();
    if !prometheus_available() {
        eprintln!("skipping: Prometheus not available at {}", prometheus_url());
        return;
    }
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
