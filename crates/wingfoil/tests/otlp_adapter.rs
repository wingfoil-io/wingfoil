//! otlp adapter tests that need no running collector — the parity port of the
//! legacy `wingfoil::adapters::otlp::push` unit tests.
//!
//! The end-to-end export test (against a live OTel collector) lives in
//! `tests/otlp_integration.rs` behind the `otlp-integration-test` feature. Run
//! these with:
//! ```sh
//! cargo test -p wingfoil --features otlp
//! ```
#![cfg(feature = "otlp")]

use std::time::Duration;

use wingfoil::adapters::otlp::{OtlpConfig, OtlpSinkOps, OtlpSpanOps};
use wingfoil::latency::{Latency, Stage, Traced, latency_stages};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

latency_stages! {
    pub TestLatency {
        a,
        b,
        c,
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct TestPayload {
    session: u64,
}

/// Parity of legacy `historical_mode_drains_without_connecting`: with no
/// collector running, a historical run must complete without any network calls
/// — the sink no-ops (never hands a value to the background exporter) under
/// `RunMode::HistoricalFrom`.
#[test]
fn historical_mode_drains_without_connecting() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = OtlpConfig::new("http://127.0.0.1:1", "test");

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .otlp_push("test_metric", config)
        .unwrap();

    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();
}

/// Parity of legacy `bad_endpoint_is_handled_gracefully`: pointing at a port
/// nothing is listening on must not panic. Export failures happen on the OTel
/// SDK's own background thread and are non-fatal, so the run result is ignored —
/// exactly as legacy does.
#[test]
fn bad_endpoint_is_handled_gracefully() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = OtlpConfig::new("http://127.0.0.1:1", "test");

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .ticker(Duration::from_millis(50))
        .count()
        .otlp_push("bad_endpoint_metric", config)
        .unwrap();

    // Errors from the background exporter are non-fatal; ignore the result.
    let _ = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
}

/// Parity of legacy `traces::historical_mode_drains_without_connecting`: an
/// `otlp_spans` sink in historical mode must complete without building a tracer
/// provider or making any network calls — the sink no-ops under
/// `RunMode::HistoricalFrom`.
#[test]
fn spans_historical_mode_drains_without_connecting() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = OtlpConfig::new("http://127.0.0.1:1", "test");

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let source = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|i: &u64| Traced::<TestPayload, TestLatency>::new(TestPayload { session: *i }));
    let _sink = source
        .otlp_spans(
            "test_span",
            config,
            |_p: &Traced<TestPayload, TestLatency>, attrs| {
                attrs.add("k", "v");
            },
        )
        .unwrap();

    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
}

/// A `(endpoint, service_name)` tuple resolves to an `OtlpConfig` via
/// `impl Into<OtlpConfig>`, so the tuple call site type-checks and drives a
/// historical (no-network) run.
#[test]
fn config_accepts_a_tuple() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .otlp_push("tuple_metric", ("http://127.0.0.1:1", "test"))
        .unwrap();

    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
}
