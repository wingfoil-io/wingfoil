//! Integration tests for the Aeron adapter.
//!
//! Requires a running Aeron media driver via Docker.
//! Run with:
//! ```sh
//! cargo test --features aeron-integration-test -p wingfoil \
//!   -- --test-threads=1 aeron::integration_tests
//! ```
//!
//! Verify the Docker image name and wait-for log string before first run:
//! ```sh
//! docker run --rm aeroncookbook/aeron-media-driver 2>&1 | head -20
//! ```

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::{Burst, Graph, RunFor, RunMode};
use std::time::Duration;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

const AERON_CHANNEL: &str = "aeron:udp?endpoint=localhost:20121";
/// Verify this image name / tag before first run.
const DRIVER_IMAGE: &str = "aeroncookbook/aeron-media-driver";
const DRIVER_TAG: &str = "latest";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Start an Aeron media driver container and return a guard that stops it on drop.
/// Uses `--network host` so the Rust test process and the container share the same
/// localhost, making `aeron:udp?endpoint=localhost:20121` reachable from both sides.
fn start_media_driver() -> anyhow::Result<impl Drop> {
    // Adjust the WaitFor string to match the actual startup log of the image.
    let container = GenericImage::new(DRIVER_IMAGE, DRIVER_TAG)
        .with_wait_for(WaitFor::message_on_stdout("INFO"))
        .with_network("host")
        .start()?;
    Ok(container)
}

// ---- Tests ----

/// Without a running media driver, `AeronHandle::connect()` must return an error.
#[test]
fn test_no_driver_connection_fails() {
    let result = AeronHandle::connect();
    assert!(
        result.is_err(),
        "expected connection error when no media driver is running"
    );
}

/// Spin-mode round-trip: an independent ticker-driven publisher sends values over the
/// media driver; the spin subscriber receives them.
///
/// Publisher and subscriber are on the same channel + stream_id, so every value the
/// publisher offers is routed back to the subscriber by the media driver.
#[cfg(feature = "aeron-rusteron")]
#[test]
fn test_spin_sub_single_message_roundtrip() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2001i32;

    // Subscribe before publishing so the media driver can pair them.
    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let subscriber = aeron_sub(
        sub,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Spin,
    );
    let collected = subscriber.collect();

    // Ticker-driven source: emit a burst of [42i64] every 10 ms.
    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(42i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    // Run pub and sub in the same graph.  pub sends independently; sub accumulates.
    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok(); // graph exits on timeout — that's expected

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&42i64),
        "expected to receive 42, got: {values:?}"
    );
    Ok(())
}

/// Spin-mode burst: publish a burst of sequential values every tick; assert at least 10
/// total values are received.
#[cfg(feature = "aeron-rusteron")]
#[test]
fn test_spin_sub_burst_accumulation() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2002i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let subscriber = aeron_sub(
        sub,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Spin,
    );
    let collected = subscriber.collect();

    // Emit a burst of 15 copies of the tick count every 10 ms.
    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|n| {
            let mut burst = Burst::new();
            for i in 0..15 {
                burst.push(n as i64 * 15 + i);
            }
            burst
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let total: usize = collected.peek_value().iter().map(|b| b.value.len()).sum();
    assert!(
        total >= 10,
        "expected at least 10 accumulated values, got: {total}"
    );
    Ok(())
}

/// Threaded-mode subscriber delivers messages via its background channel.
#[cfg(feature = "aeron-rusteron")]
#[test]
fn test_threaded_sub_delivers_messages() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2003i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let subscriber = aeron_sub(
        sub,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Threaded,
    );
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(20))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(99i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        !values.is_empty(),
        "expected at least one message via threaded subscriber"
    );
    Ok(())
}

/// Invalid (wrong-length) fragments are silently discarded by the parser.
///
/// The `i64` parser returns `None` for any buffer that is not exactly 8 bytes.
/// This test publishes one malformed 4-byte fragment and one valid 8-byte fragment
/// by calling `offer()` directly (bypassing the wingfoil pub node), then runs the
/// subscriber to collect results.
#[cfg(feature = "aeron-rusteron")]
#[test]
fn test_invalid_fragments_are_discarded() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2004i32;

    // Subscribe first so the media driver has a consumer for the seeded fragments.
    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    // Seed one malformed + one valid fragment directly via a second publication.
    {
        use crate::adapters::aeron::transport::AeronPublisherBackend;
        let mut seed_pub = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
        seed_pub.offer(&[0xDE, 0xAD, 0xBE, 0xEF])?; // 4 bytes — invalid for i64
        seed_pub.offer(&12345i64.to_le_bytes())?; // 8 bytes — valid
    }

    let subscriber = aeron_sub(
        sub,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Spin,
    );
    let collected = subscriber.collect();

    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))
        .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&12345i64),
        "expected valid value 12345, got: {values:?}"
    );
    // The 4-byte fragment must not appear as a garbage i64.
    assert!(
        !values.contains(&0x0000_0000_EFBE_ADDE_i64),
        "garbage from malformed fragment should have been discarded"
    );
    Ok(())
}

/// aeron-rs (pure-Rust) backend: ticker-driven round-trip.
#[cfg(feature = "aeron-rs")]
#[test]
fn test_aeron_rs_spin_roundtrip() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronRsHandle::connect()?;
    let stream_id = 3001i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let subscriber = aeron_sub(
        sub,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Spin,
    );
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(77i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&77i64),
        "expected 77 via aeron-rs backend, got: {values:?}"
    );
    Ok(())
}
