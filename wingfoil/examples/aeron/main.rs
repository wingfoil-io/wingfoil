//! Aeron round-trip example: publish i64 values and subscribe to them.
//!
//! Demonstrates both the spin (primary) and threaded (secondary) subscriber
//! modes against a real Aeron media driver.
//!
//! Run with:
//! ```sh
//! # Start the Aeron media driver first, then:
//! cargo run --example aeron --features aeron-rusteron
//! ```

use std::time::Duration;
use wingfoil::adapters::aeron::{AeronHandle, AeronMode, AeronPub, aeron_sub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let handle = AeronHandle::connect()?;

    let channel = "aeron:ipc";
    let stream_id = 1001i32;
    let timeout = Duration::from_secs(5);

    // -----------------------------------------------------------------------
    // Spin subscriber (primary pattern)
    // -----------------------------------------------------------------------
    println!("=== Spin subscriber ===");

    let sub = handle.subscription(channel, stream_id, timeout)?;
    let pub_ = handle.publication(channel, stream_id, timeout)?;

    let subscriber = aeron_sub(
        sub,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Spin,
    );

    // Publisher runs in its own graph on the same thread.
    let publisher_node = subscriber.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    // A downstream node that prints received values.
    let printer = subscriber.inspect(|burst| {
        for v in burst.iter() {
            println!("spin received: {v}");
        }
    });

    Graph::new(
        vec![printer.as_node(), publisher_node],
        RunMode::RealTime,
        RunFor::Cycles(10),
    )
    .run()?;

    // -----------------------------------------------------------------------
    // Threaded subscriber (secondary pattern)
    // -----------------------------------------------------------------------
    println!("\n=== Threaded subscriber ===");

    let sub2 = handle.subscription(channel, stream_id + 1, timeout)?;

    let threaded = aeron_sub(
        sub2,
        |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes),
        AeronMode::Threaded,
    );

    threaded
        .inspect(|burst| {
            for v in burst.iter() {
                println!("threaded received: {v}");
            }
        })
        .as_node()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))?;

    Ok(())
}
