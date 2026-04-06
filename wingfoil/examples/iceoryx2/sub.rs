// NOTE: iceoryx2 support is beta. Enable with the `iceoryx2-beta` feature flag.
//
// Subscribes to the counter published by `iceoryx2_pub` and demonstrates all
// three polling modes.  Pass the mode as a CLI argument:
//
//   cargo run --example iceoryx2_sub --features iceoryx2-beta -- spin
//   cargo run --example iceoryx2_sub --features iceoryx2-beta -- threaded
//   cargo run --example iceoryx2_sub --features iceoryx2-beta -- signaled
//
// Defaults to `spin` if no argument is given.
//
// Modes:
//   spin     — polls iceoryx2 directly in the graph cycle() loop.
//              Lowest latency, highest CPU (burns one core).
//   threaded — polls in a background thread, delivers via channel.
//              One channel-hop of latency, lower CPU.
//   signaled — event-driven WaitSet (true blocking).
//              Lowest CPU, highest latency. Requires the publisher to
//              signal on a matching Event service (handled automatically
//              by iceoryx2_pub).
//
// All modes use IPC (shared memory) by default.  For in-process testing,
// the adapter also supports Iceoryx2ServiceVariant::Local.
use iceoryx2::prelude::ZeroCopySend;
use log::Level::Info;
use wingfoil::adapters::iceoryx2::{Iceoryx2Mode, Iceoryx2SubOpts, iceoryx2_sub_opts};
use wingfoil::*;

/// Must match the publisher's payload type exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

fn main() {
    env_logger::init();

    let mode = match std::env::args().nth(1).as_deref() {
        Some("threaded") => Iceoryx2Mode::Threaded,
        Some("signaled") => Iceoryx2Mode::Signaled,
        Some("spin") | None => Iceoryx2Mode::Spin,
        Some(other) => {
            eprintln!("unknown mode \"{other}\" — expected spin, threaded, or signaled");
            std::process::exit(1);
        }
    };

    let service_name = "wingfoil/examples/counter";
    let opts = Iceoryx2SubOpts {
        mode,
        ..Default::default()
    };

    println!("Subscribing to \"{service_name}\" in {mode:?} mode — waiting for publisher...");

    let sub = iceoryx2_sub_opts::<Counter>(service_name, opts);
    sub.collapse()
        .inspect(|c: &Counter| println!("received seq={}", c.seq))
        .logged("sub", Info)
        .run(RunMode::RealTime, RunFor::Forever)
        .unwrap();
}
