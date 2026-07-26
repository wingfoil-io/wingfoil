//! Integration test for the otlp adapter — parity port of the classic
//! `wingfoil::adapters::otlp::integration_tests::otlp_push_sends_successfully`.
//!
//! Uses testcontainers to start an OpenTelemetry collector automatically (no
//! manual Docker setup required). The container is stopped when dropped. Run
//! with:
//! ```sh
//! cargo test -p wingfoil-next --features otlp-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "otlp-integration-test")]

use std::time::Duration;

use testcontainers::{GenericImage, core::WaitFor, runners::SyncRunner};
use wingfoil::{RunFor, RunMode};
use wingfoil_next::adapters::otlp::{OtlpConfig, OtlpSinkOps};
use wingfoil_next::prelude::*;

const COLLECTOR_IMAGE: &str = "otel/opentelemetry-collector";
const COLLECTOR_TAG: &str = "0.149.0";
const OTLP_HTTP_PORT: u16 = 4318;

/// Start an OTel collector container and return its OTLP HTTP endpoint. The
/// returned container must be kept alive for the duration of the test.
fn start_collector() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new(COLLECTOR_IMAGE, COLLECTOR_TAG)
        .with_wait_for(WaitFor::message_on_stderr("Everything is ready"))
        .start()?;
    let port = container.get_host_port_ipv4(OTLP_HTTP_PORT)?;
    Ok((container, format!("http://127.0.0.1:{port}")))
}

/// A realtime run pushes gauge values to the collector without error.
#[test]
fn otlp_push_sends_successfully() -> anyhow::Result<()> {
    let (_container, endpoint) = start_collector()?;
    let rt = tokio::runtime::Runtime::new()?;
    let config = OtlpConfig::new(endpoint, "wingfoil-next-test");

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .ticker(Duration::from_millis(100))
        .count()
        .otlp_push("wingfoil_next_integration_counter", config)?;

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(1)))?;
    Ok(())
}
