//! Cross-process IPC integration tests for the iceoryx2 adapter — a parity port
//! of the legacy `legacy/wingfoil/src/adapters/iceoryx2/integration_tests.rs`.
//!
//! These exercise the `Ipc` service variant, which uses shared memory under
//! `/dev/shm` (Linux) and a file-backed discovery layout. They require:
//!
//! - A writable `/dev/shm` (or platform equivalent) with sufficient space.
//! - The `iceoryx2-integration-test` feature flag.
//!
//! In-process `Local`-variant tests live in `tests/iceoryx2_adapter.rs` and run
//! as part of the standard `cargo test --features iceoryx2` suite.
//!
//! Run locally with:
//! ```sh
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features iceoryx2-integration-test \
//!   -- --test-threads=1
//! ```
#![cfg(feature = "iceoryx2-integration-test")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::ZeroCopySend;
use wingfoil::adapters::iceoryx2::{
    Iceoryx2ServiceVariant, Iceoryx2SinkOps, iceoryx2_sub, iceoryx2_sub_with,
};
use wingfoil::op::{Activation, Tick};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, PartialEq)]
struct TestData {
    value: u64,
}

/// Unique per test (pid + counter) so parallel tests never collide on a service.
fn unique_service_name(prefix: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "wingfoil/next/test/integration/{prefix}/{}/{n}",
        std::process::id()
    )
}

/// A late-joining subscriber receives the publisher's retained history.
///
/// iceoryx2's connection establishment is decentralized, so the out-of-graph
/// publisher must keep refreshing its connections while the graph runs for the
/// late subscriber to attach and read history. That refresh rides a busy-spin
/// `custom_node` — the next twin of legacy's
/// `IpcPublisherUpdateConnectionsNode`.
#[test]
fn late_joiner_with_history() -> anyhow::Result<()> {
    let service_name = unique_service_name("history");

    // Create a publisher and send 3 samples before the wingfoil subscriber
    // starts. Keep it alive for the graph run to avoid connection-drop races.
    let node = iceoryx2::prelude::NodeBuilder::new().create::<iceoryx2::prelude::ipc::Service>()?;
    let service = node
        .service_builder(&service_name.as_str().try_into()?)
        .publish_subscribe::<TestData>()
        .history_size(5)
        .subscriber_max_buffer_size(16)
        .open_or_create()?;
    let publisher = service.publisher_builder().create()?;
    publisher.update_connections()?;

    for i in 0..3u64 {
        let sample = publisher.loan_uninit()?;
        sample.write_payload(TestData { value: i }).send()?;
    }

    let g = GraphBuilder::new();

    // Keep refreshing the out-of-graph publisher's connections while the graph
    // runs, slowly enough to stay resilient on loaded CI runners.
    let _refresh = g.custom_node::<(), _>(&[], &[], Activation::ALWAYS, move |_ctx| {
        let _ = publisher.update_connections();
        std::thread::sleep(Duration::from_millis(5));
        Ok(Tick::Quiet)
    });

    // `collapse_accumulate` keeps *every* sample across cycles (not just the
    // last of each burst), which is what a history test needs.
    let collected = iceoryx2_sub_with::<TestData>(
        &g,
        RunMode::RealTime,
        &service_name,
        Iceoryx2ServiceVariant::Ipc,
    )?
    .collapse_accumulate();

    let mut runner = g.build();
    runner.run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(300)),
    )?;

    let values = runner.value(&collected);
    assert_eq!(values.len(), 3, "expected 3 samples from history");
    assert_eq!(values[0].value, 0);
    assert_eq!(values[1].value, 1);
    assert_eq!(values[2].value, 2);
    Ok(())
}

/// A publisher and subscriber in one graph round-trip over shared memory.
#[test]
fn ipc_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("ipc");
    let g = GraphBuilder::new();

    let collected =
        iceoryx2_sub::<TestData>(&g, RunMode::RealTime, &service_name)?.collapse_accumulate();

    let _publisher = g
        .ticker(Duration::from_millis(10))
        .map(|_: &()| burst![TestData { value: 12345 }])
        .iceoryx2_pub(&service_name);

    let mut runner = g.build();
    runner.run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(200)),
    )?;

    let values = runner.value(&collected);
    assert!(!values.is_empty());
    assert!(values.iter().all(|v| v.value == 12345));
    Ok(())
}
