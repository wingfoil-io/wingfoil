#![cfg(all(test, feature = "iceoryx2-integration-test"))]
//! Cross-process IPC integration tests for the iceoryx2 adapter.
//!
//! These exercise the `Ipc` service variant, which uses shared memory under
//! `/dev/shm` (Linux) and a file-backed discovery layout. They require:
//!
//! - A writable `/dev/shm` (or platform equivalent) with sufficient space.
//! - The `iceoryx2-integration-test` feature flag.
//!
//! In-process `Local`-variant tests live in `local_tests.rs` and run as part
//! of the standard `cargo test --features iceoryx2-beta` suite.
//!
//! Run locally with:
//! ```sh
//! cargo test -p wingfoil --features iceoryx2-integration-test \
//!   -- --test-threads=1 iceoryx2::integration_tests
//! ```

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::types::{Burst, IntoNode};
use crate::{Graph, RunFor, RunMode, ticker};
use iceoryx2::port::update_connections::UpdateConnections;
use iceoryx2::prelude::ZeroCopySend;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, PartialEq)]
struct TestData {
    value: u64,
}

fn unique_service_name(prefix: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "wingfoil/test/integration/{prefix}/{}/{n}",
        std::process::id()
    )
}

struct IpcPublisherUpdateConnectionsNode {
    publisher: iceoryx2::port::publisher::Publisher<iceoryx2::prelude::ipc::Service, TestData, ()>,
    cycles: u64,
}

impl crate::MutableNode for IpcPublisherUpdateConnectionsNode {
    fn cycle(&mut self, _state: &mut crate::GraphState) -> anyhow::Result<bool> {
        self.cycles += 1;
        // In decentralized discovery, connections can take some wall-clock time to converge.
        // Slowing this node down keeps the IPC test resilient on loaded CI runners and under
        // filesystem-backed discovery delays.
        let _ = self.publisher.update_connections();
        std::thread::sleep(Duration::from_millis(5));
        Ok(false)
    }

    fn start(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        state.always_callback();
        let _ = self.publisher.update_connections();
        Ok(())
    }
}

#[test]
fn test_late_joiner_with_history() -> anyhow::Result<()> {
    let service_name = unique_service_name("history");

    // Create a publisher and send 3 samples before the Wingfoil subscriber starts.
    // Keep the publisher alive for the duration of the graph run to avoid connection-drop races.
    let node = iceoryx2::prelude::NodeBuilder::new().create::<iceoryx2::prelude::ipc::Service>()?;
    let service = node
        .service_builder(&service_name.as_str().try_into()?)
        .publish_subscribe::<TestData>()
        .history_size(5)
        .subscriber_max_buffer_size(16)
        .open_or_create()?;
    let publisher = service.publisher_builder().create()?;
    publisher.update_connections()?;

    for i in 0..3 {
        let sample = publisher.loan_uninit()?;
        sample.write_payload(TestData { value: i as u64 }).send()?;
    }

    // Start Wingfoil subscriber and verify it can receive the history.
    let sub = iceoryx2_sub_with::<TestData>(&service_name, Iceoryx2ServiceVariant::Ipc);
    // Note: `collapse()` keeps only the last item of a burst. For a history test we want *all*
    // samples that arrive across cycles, so we fold bursts into a flat vector.
    let collected = sub.fold(Box::new(|acc: &mut Vec<TestData>, burst| {
        for item in burst {
            acc.push(item);
        }
    }));

    // Connection establishment in iceoryx2 is decentralized. When the subscriber joins late,
    // the publisher must also refresh connections while the graph is running so the subscriber
    // can connect and read history.
    let publisher_update = IpcPublisherUpdateConnectionsNode {
        publisher,
        cycles: 0,
    }
    .into_node();

    Graph::new(
        vec![publisher_update, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(300)),
    )
    .run()?;

    let values = collected.peek_value();
    assert_eq!(values.len(), 3, "expected 3 samples from history");
    assert_eq!(values[0].value, 0);
    assert_eq!(values[1].value, 1);
    assert_eq!(values[2].value, 2);
    Ok(())
}

#[test]
fn test_ipc_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("ipc");

    let sub = iceoryx2_sub::<TestData>(&service_name);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(10)).produce(|| {
        let mut b = Burst::default();
        b.push(TestData { value: 12345 });
        b
    });
    let pub_node = iceoryx2_pub(upstream, &service_name);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(200)),
    )
    .run()?;

    let values = collected.peek_value();
    assert!(!values.is_empty());
    assert!(values.iter().all(|v| v.value.value == 12345));
    Ok(())
}
