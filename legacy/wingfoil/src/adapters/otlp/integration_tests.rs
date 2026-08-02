//! Integration tests for the OTLP adapter.
//!
//! Uses testcontainers to start an OpenTelemetry collector automatically
//! (no manual Docker setup required).
//!
//! Run with:
//! ```sh
//! RUST_LOG=INFO cargo test --features otlp-integration-test -p wingfoil -- --test-threads=1 --nocapture adapters::otlp::integration_tests
//! ```

use crate::adapters::otlp::{OtlpConfig, OtlpPush, OtlpSpans};
use crate::latency::{Latency, Stage};
use crate::{RunFor, RunMode, Traced, nodes::*};
use std::time::Duration;
use testcontainers::{GenericImage, core::WaitFor, runners::SyncRunner};

crate::latency_stages! {
    pub IntegrationLatency {
        ingress,
        process,
        egress,
    }
}

fn start_collector() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new("otel/opentelemetry-collector", "0.149.0")
        .with_wait_for(WaitFor::message_on_stderr("Everything is ready"))
        .start()?;
    let port = container.get_host_port_ipv4(4318)?;
    Ok((container, format!("http://127.0.0.1:{port}")))
}

#[test]
fn otlp_push_sends_successfully() -> anyhow::Result<()> {
    _ = env_logger::try_init();
    let (_container, endpoint) = start_collector()?;
    let config = OtlpConfig {
        endpoint,
        service_name: "wingfoil-test".into(),
    };
    let counter = ticker(Duration::from_millis(100)).count();
    let node = counter.otlp_push("wingfoil_integration_counter", config);
    node.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(1)))?;
    Ok(())
}

#[test]
fn otlp_spans_sends_successfully() -> anyhow::Result<()> {
    _ = env_logger::try_init();
    let (_container, endpoint) = start_collector()?;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    struct TestPayload {
        session: u64,
    }

    let config = OtlpConfig {
        endpoint,
        service_name: "wingfoil-test-spans".into(),
    };

    let counter = ticker(Duration::from_millis(100)).count().map(|i: u64| {
        let mut traced = Traced::<TestPayload, IntegrationLatency>::new(TestPayload { session: i });
        *traced.latency.stamp_mut(0) = 1000; // ingress
        *traced.latency.stamp_mut(1) = 2000; // process
        *traced.latency.stamp_mut(2) = 3000; // egress
        traced
    });

    let node = counter.otlp_spans(config, "integration_span", |p, attrs| {
        attrs.add("session_id", p.payload.session.to_string());
    });

    node.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(1)))?;
    Ok(())
}
