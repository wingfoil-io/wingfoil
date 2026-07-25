//! prometheus adapter tests that need no running service — the parity port of
//! the classic `wingfoil::adapters::prometheus::exporter` unit tests (plus the
//! self-contained `multiple_metrics` integration test).
//!
//! The end-to-end scrape test (a live Prometheus scraping the exporter) lives in
//! `tests/prometheus_integration.rs` behind the `prometheus-integration-test`
//! feature. Run these with:
//! ```sh
//! cargo test -p wingfoil-next --features prometheus
//! ```
#![cfg(feature = "prometheus")]

use std::io::{Read, Write};
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::prometheus::{PrometheusExporter, PrometheusSinkOps};
use wingfoil_next::prelude::*;

/// Scrape `GET /metrics` over a raw TCP connection, returning the response body
/// (headers stripped). Retries briefly in case the server thread has not bound
/// yet. Port `0` binds are OS-assigned, so parallel tests never collide.
fn get_metrics(port: u16) -> String {
    for _ in 0..20 {
        if let Ok(mut conn) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
            conn.write_all(b"GET /metrics HTTP/1.0\r\n\r\n").unwrap();
            let mut response = String::new();
            conn.read_to_string(&mut response).unwrap();
            if let Some(pos) = response.find("\r\n\r\n") {
                return response[pos + 4..].to_string();
            }
            return response;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("could not connect to metrics server on port {port}");
}

/// Parity of classic `connection_refused_when_port_occupied`: binding a port
/// another listener already holds must fail at `serve()`, before any run.
#[test]
fn connection_refused_when_port_occupied() {
    let occupied =
        std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind test listener");
    let port = occupied.local_addr().unwrap().port();
    let exporter = PrometheusExporter::new(format!("127.0.0.1:{port}"));
    let result = exporter.serve();
    assert!(result.is_err(), "expected bind error when port is occupied");
}

/// Parity of classic `serves_registered_metric`: a realtime run publishes the
/// stringified stream value into the scrape body, with the `# TYPE … gauge`
/// header.
#[test]
fn serves_registered_metric() {
    let exporter = PrometheusExporter::new("127.0.0.1:0");
    let port = exporter.serve().unwrap();

    let g = GraphBuilder::new();
    let _metric = g
        .ticker(Duration::from_millis(10))
        .count()
        .prometheus_gauge(&exporter, "test_counter");

    g.build().run(RunMode::RealTime, RunFor::Cycles(5)).unwrap();

    let body = get_metrics(port);
    assert!(
        body.contains("test_counter 5"),
        "expected 'test_counter 5' in:\n{body}"
    );
    assert!(
        body.contains("# TYPE test_counter gauge"),
        "expected TYPE line in:\n{body}"
    );
}

/// Parity of classic `historical_mode_produces_no_metrics`: under
/// `RunMode::HistoricalFrom` the sink is a no-op, so the scrape body is empty
/// (the slot is never written, so it is omitted).
#[test]
fn historical_mode_produces_no_metrics() {
    let exporter = PrometheusExporter::new("127.0.0.1:0");
    let port = exporter.serve().unwrap();

    let g = GraphBuilder::new();
    let _metric = g
        .ticker(Duration::from_millis(10))
        .count()
        .prometheus_gauge(&exporter, "hist_counter");

    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();

    let body = get_metrics(port);
    assert!(
        body.is_empty(),
        "expected no metrics in historical mode, got:\n{body}"
    );
}

/// Parity of classic `returns_404_for_unknown_path`: any path other than
/// `GET /metrics` gets a 404.
#[test]
fn returns_404_for_unknown_path() {
    let exporter = PrometheusExporter::new("127.0.0.1:0");
    let port = exporter.serve().unwrap();
    std::thread::sleep(Duration::from_millis(50));

    let mut conn = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    conn.write_all(b"GET /other HTTP/1.0\r\n\r\n").unwrap();
    let mut response = String::new();
    conn.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 404"),
        "expected 404, got: {response}"
    );
}

/// Parity of the classic self-contained `multiple_metrics_all_appear_in_scrape`
/// integration test: two metrics sharing one graph both appear in the scrape.
#[test]
fn multiple_metrics_all_appear_in_scrape() {
    let exporter = PrometheusExporter::new("127.0.0.1:0");
    let port = exporter.serve().unwrap();

    let g = GraphBuilder::new();
    let counter = g.ticker(Duration::from_millis(10)).count();
    let doubled = counter.map(|n: &u64| n * 2);

    let _a = counter.prometheus_gauge(&exporter, "multi_counter_a");
    let _b = doubled.prometheus_gauge(&exporter, "multi_counter_b");

    g.build().run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();

    let body = get_metrics(port);
    assert!(
        body.contains("multi_counter_a"),
        "missing multi_counter_a in:\n{body}"
    );
    assert!(
        body.contains("multi_counter_b"),
        "missing multi_counter_b in:\n{body}"
    );
}
