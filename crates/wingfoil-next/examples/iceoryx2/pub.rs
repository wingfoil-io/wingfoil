//! iceoryx2 publisher example — publishes a counter over shared memory.
//!
//! A port of the classic `wingfoil/examples/iceoryx2/pub.rs` onto the next
//! engine. Start the subscriber first (`iceoryx2_sub`), then run this publisher.
//!
//! # Run
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil-next --example iceoryx2_pub --features iceoryx2
//! ```

use std::time::Duration;

use iceoryx2::prelude::ZeroCopySend;
use log::Level::Info;
use wingfoil_next::adapters::iceoryx2::Iceoryx2SinkOps;
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

/// Payload must be `#[repr(C)]` and implement `ZeroCopySend` for zero-copy IPC.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let service_name = "wingfoil/examples/counter";
    let period = Duration::from_millis(100);

    let g = GraphBuilder::new();
    let _publisher = g
        .ticker(period)
        .count()
        .map(|seq: &u64| burst![Counter { seq: *seq }])
        .logged("pub", Info)
        .iceoryx2_pub(service_name);

    println!("Publishing Counter on \"{service_name}\" every {period:?} — press Ctrl-C to stop");
    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}
