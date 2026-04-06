// NOTE: iceoryx2 support is beta. Enable with the `iceoryx2-beta` feature flag.
// Run with: RUST_LOG=info cargo run --example iceoryx2_sub --features iceoryx2-beta
//
// This subscribes to the counter published by `iceoryx2_pub`.
// Start this subscriber first, then run the publisher.
use iceoryx2::prelude::ZeroCopySend;
use log::Level::Info;
use wingfoil::adapters::iceoryx2::iceoryx2_sub;
use wingfoil::*;

/// Must match the publisher's payload type exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

fn main() {
    env_logger::init();

    let service_name = "wingfoil/examples/counter";

    println!("Subscribing to \"{service_name}\" — waiting for publisher...");

    let sub = iceoryx2_sub::<Counter>(service_name);
    sub.collapse()
        .inspect(|c: &Counter| println!("received seq={}", c.seq))
        .logged("sub", Info)
        .run(RunMode::RealTime, RunFor::Forever)
        .unwrap();
}
