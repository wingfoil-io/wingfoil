//! prometheus adapter end-to-end integration test — a live Prometheus scraping
//! the exporter. The parity port of the classic
//! `prometheus::integration_tests::prometheus_exporter_scrapeable_by_prometheus`.
//!
//! Reuses the classic Docker stack (which scrapes `WINGFOIL_METRICS_PORT`, default
//! `9091`, on the host):
//! ```sh
//! docker compose -f wingfoil/src/adapters/prometheus/docker/docker-compose.yml up -d
//! cargo test -p wingfoil-next --features prometheus-integration-test -- --test-threads=1 --nocapture
//! ```
//! The test skips itself (prints and returns) if Prometheus is unreachable.
#![cfg(feature = "prometheus-integration-test")]

use std::time::Duration;

use wingfoil::{RunFor, RunMode};
use wingfoil_next::adapters::prometheus::{PrometheusExporter, PrometheusSinkOps};
use wingfoil_next::prelude::*;

fn prometheus_url() -> String {
    std::env::var("PROMETHEUS_TEST_URL").unwrap_or_else(|_| "http://localhost:9090".into())
}

fn metrics_port() -> u16 {
    std::env::var("WINGFOIL_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9091)
}

fn prometheus_available() -> bool {
    reqwest::blocking::get(format!("{}/api/v1/query?query=up", prometheus_url()))
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[test]
fn prometheus_exporter_scrapeable_by_prometheus() {
    if !prometheus_available() {
        eprintln!("skipping: Prometheus not available at {}", prometheus_url());
        return;
    }
    let port = metrics_port();
    let exporter = PrometheusExporter::new(format!("0.0.0.0:{port}"));
    exporter.serve().unwrap();

    let g = GraphBuilder::new();
    let _metric = g
        .ticker(Duration::from_millis(100))
        .count()
        .prometheus_gauge(&exporter, "wingfoil_integration_counter");

    // Run long enough for Prometheus to complete at least one scrape (interval = 5s).
    g.build()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(7)))
        .unwrap();

    // Poll Prometheus until the metric appears (scrape may lag by up to one
    // interval). Parse the vector query response by text — an empty match is
    // `"result":[]`, a hit names the metric — so the test needs only `reqwest`,
    // no JSON crate.
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
        let body = resp.text().expect("Prometheus response was not text");
        if body.contains("wingfoil_integration_counter") && !body.contains("\"result\":[]") {
            println!("Prometheus scrape hit (attempt {attempt}): {body}");
            found = true;
            break;
        }
        println!("attempt {attempt}: metric not yet in Prometheus, retrying...");
        std::thread::sleep(Duration::from_secs(5));
    }
    assert!(
        found,
        "wingfoil_integration_counter never appeared in Prometheus after polling"
    );
}
