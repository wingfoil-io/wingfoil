// NOTE: iceoryx2 support is beta. Enable with the `iceoryx2-beta` feature flag.
// Run with: RUST_LOG=info cargo run --example iceoryx2_pub --features iceoryx2-beta
//
// This publishes a simple counter over iceoryx2 shared memory.
// Start the subscriber first (`iceoryx2_sub`), then run this publisher.
use iceoryx2::prelude::ZeroCopySend;
use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::iceoryx2::iceoryx2_pub;
use wingfoil::*;

/// Payload must be `#[repr(C)]` and implement `ZeroCopySend` for zero-copy IPC.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

fn main() {
    env_logger::init();

    let service_name = "wingfoil/examples/counter";
    let period = Duration::from_millis(100);

    let upstream = ticker(period)
        .count()
        .map(|seq: u64| {
            let mut b = Burst::default();
            b.push(Counter { seq });
            b
        })
        .logged("pub", Info);

    let pub_node = iceoryx2_pub(upstream, service_name);

    println!("Publishing Counter on \"{service_name}\" every {period:?} — press Ctrl-C to stop");
    Graph::new(vec![pub_node], RunMode::RealTime, RunFor::Forever)
        .run()
        .unwrap();
}
