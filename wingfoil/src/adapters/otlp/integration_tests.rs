//! Integration tests for the OTLP adapter.
//!
//! Requires a running OpenTelemetry collector. Start one with:
//! ```sh
//! docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.116.0
//! ```
//!
//! Or use testcontainers (started automatically by the tests below).
//!
//! Run with:
//! ```sh
//! RUST_LOG=INFO cargo test --features otlp-integration-test -p wingfoil -- --test-threads=1 --nocapture
//! ```

use crate::adapters::otlp::{OtlpConfig, OtlpPush};
use crate::{RunFor, RunMode, nodes::*};
use std::time::Duration;
use testcontainers::{GenericImage, core::WaitFor, runners::SyncRunner};

fn start_collector() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new("otel/opentelemetry-collector", "0.116.0")
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
