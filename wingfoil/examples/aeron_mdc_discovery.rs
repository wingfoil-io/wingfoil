//! End-to-end named-discovery round-trip over Multi-Destination-Cast (MDC).
//!
//! Demonstrates the Story-12.6 wingfoil surfaces working in concert:
//! - [`ChannelUri::mdc_publication`](wingfoil::adapters::aeron::ChannelUri::mdc_publication)
//!   and [`ChannelUri::mdc_subscription`](wingfoil::adapters::aeron::ChannelUri::mdc_subscription)
//!   build the canonical MDC URI strings.
//! - [`register_pub`](wingfoil::adapters::aeron::register_pub) and
//!   [`register_sub`](wingfoil::adapters::aeron::register_sub) populate the
//!   process-global discovery registry.
//! - [`lookup_pub`](wingfoil::adapters::aeron::lookup_pub) /
//!   [`lookup_sub`](wingfoil::adapters::aeron::lookup_sub) round-trip the
//!   registered tuple so the wingfoil application code never hand-builds the
//!   URI string.
//! - [`aeron_pub_named`](wingfoil::adapters::aeron::aeron_pub_named) is the
//!   publisher-side pass-through guard.
//! - [`aeron_sub_burst_named`](wingfoil::adapters::aeron::aeron_sub_burst_named)
//!   is the subscriber-side factory wrapper that builds the burst stream
//!   after asserting `name` is registered.
//!
//! Run with: `cargo run --example aeron_mdc_discovery --features aeron-rusteron`
//!
//! Note: re-running in the same process will surface
//! `Err(DiscoveryError::AlreadyRegistered)`-style behaviour because the
//! discovery registry is process-global. The current registry semantics
//! treat re-registration as "last write wins", so this example is safe to
//! re-run in fresh processes.

use std::time::Duration;
use wingfoil::adapters::aeron::buffer::FragmentBuffer;
use wingfoil::adapters::aeron::error::TransportError;
use wingfoil::adapters::aeron::{
    AeronHandle, AeronPub, AeronSubOptions, ChannelUri, aeron_pub_named, aeron_sub_burst_named,
    lookup_pub, lookup_sub, register_pub, register_sub,
};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // 1. Build the canonical MDC URIs via ChannelUri::mdc_*.
    let pub_uri = ChannelUri::mdc_publication("127.0.0.1:40456")?;
    let sub_uri = ChannelUri::mdc_subscription("127.0.0.1:40457", "127.0.0.1:40456")?;
    let stream_id = 9101i32;

    println!("MDC publication URI : {pub_uri}");
    println!("MDC subscription URI: {sub_uri}");

    // 2. Register both names with the process-global discovery registry.
    register_pub("positions", pub_uri.clone(), stream_id)
        .map_err(|e| anyhow::anyhow!("register_pub failed: {e}"))?;
    register_sub("positions", sub_uri.clone(), stream_id)
        .map_err(|e| anyhow::anyhow!("register_sub failed: {e}"))?;

    // 3. Round-trip the registered tuple — proves the wingfoil application
    //    never hand-builds the URI string after startup wiring.
    let (resolved_pub_uri, resolved_pub_stream) =
        lookup_pub("positions").expect("just-registered name resolves");
    let (resolved_sub_uri, resolved_sub_stream) =
        lookup_sub("positions").expect("just-registered name resolves");
    assert_eq!(resolved_pub_uri, pub_uri);
    assert_eq!(resolved_pub_stream, stream_id);
    assert_eq!(resolved_sub_uri, sub_uri);
    assert_eq!(resolved_sub_stream, stream_id);

    // 4. Acquire concrete rusteron backends using the resolved tuple.
    let handle = AeronHandle::connect()?;
    let timeout = Duration::from_secs(5);
    let rusteron_pub = handle.publication(&resolved_pub_uri, resolved_pub_stream, timeout)?;
    let rusteron_sub = handle.subscription(&resolved_sub_uri, resolved_sub_stream, timeout)?;

    // 5. Wrap the publisher with the name-checked pass-through guard.
    let guarded_pub = aeron_pub_named("positions", rusteron_pub)
        .map_err(|e| anyhow::anyhow!("aeron_pub_named: {e}"))?;

    // 6. Build the subscriber burst stream via the name-checked factory.
    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let sub_stream = aeron_sub_burst_named::<i64, _, _>(
        "positions",
        rusteron_sub,
        parser,
        AeronSubOptions::default(),
    )
    .map_err(|e| anyhow::anyhow!("aeron_sub_burst_named: {e}"))?;

    // 7. Publish 5 values from a ticker, inspect on the receive side.
    let printer = sub_stream.inspect(|burst| {
        for v in burst.iter() {
            println!("mdc received: {v}");
        }
    });

    let publisher_node = wingfoil::ticker(Duration::from_millis(300))
        .count()
        .map(|n: u64| {
            let mut b: Burst<i64> = Burst::new();
            b.push(n as i64);
            b
        })
        .aeron_pub(guarded_pub, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![printer.as_node(), publisher_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(3)),
    )
    .run()?;

    Ok(())
}
