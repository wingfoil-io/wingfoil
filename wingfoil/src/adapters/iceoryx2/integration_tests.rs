#![cfg(test)]
//! Integration tests for the iceoryx2 adapter.

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::types::Burst;
use crate::{Graph, RunFor, RunMode, ticker};
#[cfg(feature = "iceoryx2-integration-test")]
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

#[test]
fn test_local_spin_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("spin");

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Spin,
        ..Default::default()
    };
    let sub = iceoryx2_sub_opts::<TestData>(&service_name, opts);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<TestData> = Burst::default();
        b.push(TestData { value: 42 });
        b
    });
    let pub_node = iceoryx2_pub_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(100)),
    )
    .run()?;

    let values = collected.peek_value();
    assert!(!values.is_empty(), "expected to receive samples");
    assert!(values.iter().all(|v| v.value.value == 42));
    Ok(())
}

#[test]
fn test_local_threaded_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("threaded");

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Threaded,
        ..Default::default()
    };
    let sub = iceoryx2_sub_opts::<TestData>(&service_name, opts);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<TestData> = Burst::default();
        b.push(TestData { value: 99 });
        b
    });
    let pub_node = iceoryx2_pub_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        // Background threads + connection establishment can be slow on loaded CI runners.
        RunFor::Duration(Duration::from_millis(500)),
    )
    .run()?;

    // Threaded mode might take a cycle or two to deliver
    let values = collected.peek_value();
    assert!(
        !values.is_empty(),
        "expected to receive samples in threaded mode"
    );
    assert!(values.iter().all(|v| v.value.value == 99));
    Ok(())
}

#[test]
fn test_local_signaled_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("signaled");

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Signaled,
        ..Default::default()
    };
    let sub = iceoryx2_sub_opts::<TestData>(&service_name, opts);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<TestData> = Burst::default();
        b.push(TestData { value: 7 });
        b
    });
    let pub_node = iceoryx2_pub_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        // Signaled mode depends on the Event service and can require a few retries on startup.
        RunFor::Duration(Duration::from_millis(500)),
    )
    .run()?;

    let values = collected.peek_value();
    assert!(
        !values.is_empty(),
        "expected to receive samples in signaled mode"
    );
    assert!(values.iter().all(|v| v.value.value == 7));
    Ok(())
}

#[test]
fn test_local_service_config_mismatch_fails() {
    // `history_size` is part of iceoryx2 publish/subscribe service configuration.
    // If publisher and subscriber disagree, `open_or_create()` should error.
    let service_name = unique_service_name("mismatch");

    let sub = iceoryx2_sub_opts::<TestData>(
        &service_name,
        Iceoryx2SubOpts {
            variant: Iceoryx2ServiceVariant::Local,
            mode: Iceoryx2Mode::Spin,
            history_size: 7,
        },
    );

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<TestData> = Burst::default();
        b.push(TestData { value: 1 });
        b
    });

    let pub_node = iceoryx2_pub_opts(
        upstream,
        &service_name,
        Iceoryx2PubOpts {
            variant: Iceoryx2ServiceVariant::Local,
            history_size: 5,
        },
    );

    let collected = sub.collapse().collect();
    let res = Graph::new(
        vec![pub_node, collected.as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(100)),
    )
    .run();

    assert!(res.is_err(), "expected mismatch to fail");
    let err = res.unwrap_err();
    let ice_err = err
        .downcast_ref::<Iceoryx2Error>()
        .expect("expected an Iceoryx2Error");
    match ice_err {
        Iceoryx2Error::ServiceOpenFailed {
            service_name: s,
            variant,
            history_size,
            subscriber_max_buffer_size,
            ..
        } => {
            assert_eq!(s, &service_name);
            assert_eq!(variant, &Iceoryx2ServiceVariant::Local);
            assert_eq!(*history_size, 7);
            assert!(*subscriber_max_buffer_size >= 16);
        }
        Iceoryx2Error::ServiceConfigMismatch {
            service_name: s,
            variant,
            history_size,
            subscriber_max_buffer_size,
            ..
        } => {
            assert_eq!(s, &service_name);
            assert_eq!(variant, &Iceoryx2ServiceVariant::Local);
            assert_eq!(*history_size, 7);
            assert!(*subscriber_max_buffer_size >= 16);
        }
        other => panic!("expected ServiceOpenFailed, got {other:?}"),
    }
}

#[test]
fn test_local_slice_spin_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("slice/spin");

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Spin,
        ..Default::default()
    };
    let sub = iceoryx2_sub_slice_opts(&service_name, opts);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<Vec<u8>> = Burst::default();
        b.push(b"abc".to_vec());
        b
    });
    let pub_node = iceoryx2_pub_slice_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(150)),
    )
    .run()?;

    let values: Vec<Vec<u8>> = collected
        .peek_value()
        .into_iter()
        .map(|item| item.value)
        .collect();
    assert!(!values.is_empty(), "expected slice samples");
    assert!(values.iter().all(|v| v.as_slice() == b"abc"));
    Ok(())
}

#[test]
fn test_local_slice_threaded_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("slice/threaded");

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Threaded,
        ..Default::default()
    };
    let sub = iceoryx2_sub_slice_opts(&service_name, opts);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<Vec<u8>> = Burst::default();
        b.push(b"def".to_vec());
        b
    });
    let pub_node = iceoryx2_pub_slice_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(500)),
    )
    .run()?;

    let values: Vec<Vec<u8>> = collected
        .peek_value()
        .into_iter()
        .map(|item| item.value)
        .collect();
    assert!(
        !values.is_empty(),
        "expected slice samples in threaded mode"
    );
    assert!(values.iter().all(|v| v.as_slice() == b"def"));
    Ok(())
}

#[test]
fn test_local_slice_signaled_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("slice/signaled");

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Signaled,
        ..Default::default()
    };
    let sub = iceoryx2_sub_slice_opts(&service_name, opts);
    let collected = sub.collapse().collect();

    let upstream = ticker(Duration::from_millis(5)).produce(|| {
        let mut b: Burst<Vec<u8>> = Burst::default();
        b.push(b"ghi".to_vec());
        b
    });
    let pub_node = iceoryx2_pub_slice_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(500)),
    )
    .run()?;

    let values: Vec<Vec<u8>> = collected
        .peek_value()
        .into_iter()
        .map(|item| item.value)
        .collect();
    assert!(
        !values.is_empty(),
        "expected slice samples in signaled mode"
    );
    assert!(values.iter().all(|v| v.as_slice() == b"ghi"));
    Ok(())
}

#[cfg(feature = "iceoryx2-integration-test")]
#[ignore = "Requires IPC shared memory environment; run explicitly with -- --ignored"]
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
    let collected = sub.collapse().collect();

    Graph::new(
        vec![collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Cycles(50),
    )
    .run()?;

    let values = collected.peek_value();
    assert_eq!(values.len(), 3, "expected 3 samples from history");
    assert_eq!(values[0].value.value, 0);
    assert_eq!(values[1].value.value, 1);
    assert_eq!(values[2].value.value, 2);
    Ok(())
}

#[cfg(feature = "iceoryx2-integration-test")]
#[ignore = "Requires IPC shared memory environment; run explicitly with -- --ignored"]
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
