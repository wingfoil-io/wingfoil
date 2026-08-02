//! Aeron round-trip example: publish `i64` values and subscribe to them.
//!
//! A port of the legacy `legacy/wingfoil/examples/aeron/main.rs` onto the next engine.
//! Demonstrates both the spin (primary) and threaded (secondary) subscriber
//! modes against a real Aeron media driver.
//!
//! Run with:
//! ```sh
//! # Start the Aeron media driver first, then:
//! cargo run -p wingfoil-next --example aeron_adapter --features aeron
//! ```

use std::time::Duration;

use wingfoil_next::adapters::aeron::{
    AeronHandle, AeronMode, AeronSinkOps, AeronSubOptions, FragmentBuffer, TransportError,
    aeron_sub_fragment,
};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

fn i64_parser(f: &FragmentBuffer<'_>) -> Result<Option<i64>, TransportError> {
    Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
}

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
    {
        let g = GraphBuilder::new();
        let received = aeron_sub_fragment(
            &g,
            RunMode::RealTime,
            handle.subscription(channel, stream_id, timeout)?,
            i64_parser,
            AeronSubOptions {
                mode: AeronMode::Spin,
                ..Default::default()
            },
        )?;

        // Echo whatever arrives straight back onto the same stream id, and print
        // it on the way past.
        let _publisher = received.aeron_pub(
            handle.publication(channel, stream_id, timeout)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );
        let _printer = received.inspect(|burst: &Burst<i64>| {
            for v in burst.iter() {
                println!("spin received: {v}");
            }
        });

        g.build().run(RunMode::RealTime, RunFor::Cycles(10))?;
    }

    // -----------------------------------------------------------------------
    // Threaded subscriber (secondary pattern)
    // -----------------------------------------------------------------------
    println!("\n=== Threaded subscriber ===");
    {
        let g = GraphBuilder::new();
        let _printer = aeron_sub_fragment(
            &g,
            RunMode::RealTime,
            handle.subscription(channel, stream_id + 1, timeout)?,
            i64_parser,
            AeronSubOptions {
                mode: AeronMode::Threaded,
                ..Default::default()
            },
        )?
        .inspect(|burst: &Burst<i64>| {
            for v in burst.iter() {
                println!("threaded received: {v}");
            }
        });

        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))?;
    }

    Ok(())
}
