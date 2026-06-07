//! Aeron round-trip example: publish i64 values and subscribe to them.
//!
//! Demonstrates both the spin (primary) and threaded (secondary) subscriber
//! modes against a real Aeron media driver.
//!
//! Run with:
//! ```sh
//! # Start the Aeron media driver first, then:
//! cargo run --example aeron --features aeron
//! ```

use std::time::Duration;
use wingfoil::adapters::aeron::{
    AeronHandle, AeronMode, AeronPub, AeronSubOptions, FragmentBuffer, TransportError,
    aeron_sub_fragment,
};
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

    let subscriber = aeron_sub_fragment(
        sub,
        |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
        },
        AeronSubOptions {
            mode: AeronMode::Spin,
            ..Default::default()
        },
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

    let threaded = aeron_sub_fragment(
        sub2,
        |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
        },
        AeronSubOptions {
            mode: AeronMode::Threaded,
            ..Default::default()
        },
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
