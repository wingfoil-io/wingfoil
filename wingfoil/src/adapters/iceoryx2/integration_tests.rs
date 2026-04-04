#![cfg(test)]
//! Integration tests for the iceoryx2 adapter.

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::types::Burst;
use crate::{Graph, RunFor, RunMode, ticker};
use iceoryx2::prelude::ZeroCopySend;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, PartialEq)]
struct TestData {
    value: u64,
}

#[test]
fn test_local_spin_round_trip() -> anyhow::Result<()> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let service_name = format!("wingfoil/test/integration/spin/{}/{n}", std::process::id());

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Spin,
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
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let service_name = format!(
        "wingfoil/test/integration/threaded/{}/{n}",
        std::process::id()
    );

    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode: Iceoryx2Mode::Threaded,
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
        RunFor::Duration(Duration::from_millis(200)), // Give it a bit more time for threaded
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
