//! Local (in-process) tests for the iceoryx2 adapter.
//!
//! These exercise the `Local` service variant end-to-end — no shared memory,
//! no subprocesses. They run as part of the standard
//! `cargo test -p wingfoil --features iceoryx2-beta` suite.
//!
//! The cross-process IPC tests live in `integration_tests.rs` and are gated by
//! the `iceoryx2-integration-test` feature.

use super::*;
use crate::latency::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::types::Burst;
use crate::{Graph, RunFor, RunMode, latency_stages, ticker};
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
    format!("wingfoil/test/local/{prefix}/{}/{n}", std::process::id())
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

// ── Latency: end-to-end across an iceoryx2 Local hop ────────────────────────

latency_stages! {
    pub TestLatency {
        produce,
        publish,
        receive,
        ack,
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, PartialEq)]
struct TestQuote {
    seq: u64,
}

/// Stamp `produce` and `publish` on the way out, ship a
/// `Traced<TestQuote, TestLatency>` over the iceoryx2 Local hop, stamp
/// `receive` and `ack` on the way in, and confirm that all four stamps land
/// in monotonic order — proving the Traced wrapper crosses the IPC boundary
/// intact.
#[test]
fn test_local_latency_round_trip() -> anyhow::Result<()> {
    let service_name = unique_service_name("latency");

    // Subscriber side: stamp receive and ack, collect for assertion.
    let sub = iceoryx2_sub_with::<Traced<TestQuote, TestLatency>>(
        &service_name,
        Iceoryx2ServiceVariant::Local,
    );
    let collected = sub
        .collapse::<Traced<TestQuote, TestLatency>>()
        .stamp::<test_latency::receive>()
        .stamp::<test_latency::ack>()
        .collect();

    // Publisher side: build payload, stamp produce and publish.
    let upstream = ticker(Duration::from_millis(5))
        .count()
        .map(|seq: u64| Traced::<TestQuote, TestLatency>::new(TestQuote { seq }))
        .stamp::<test_latency::produce>()
        .stamp::<test_latency::publish>()
        .map(|t| {
            let mut b: Burst<Traced<TestQuote, TestLatency>> = Burst::default();
            b.push(t);
            b
        });
    let pub_node = iceoryx2_pub_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

    Graph::new(
        vec![pub_node, collected.clone().as_node()],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(150)),
    )
    .run()?;

    let values = collected.peek_value();
    assert!(!values.is_empty(), "expected to receive at least one quote");
    for v in &values {
        let l = &v.value.latency;
        // All four stamps must be set and monotonically non-decreasing.
        assert!(l.produce > 0, "produce stamp missing");
        assert!(l.publish >= l.produce, "publish < produce");
        assert!(l.receive >= l.publish, "receive < publish");
        assert!(l.ack >= l.receive, "ack < receive");
    }

    // Aggregating the stamps via LatencyStats should produce non-zero counts
    // for every adjacent stage transition.
    let mut stats = LatencyStats::<TestLatency>::new();
    for v in &values {
        stats.observe(&v.value.latency);
    }
    for i in 1..TestLatency::N {
        assert!(
            stats.stages[i].count > 0,
            "stage {i} never observed a delta"
        );
    }
    Ok(())
}
