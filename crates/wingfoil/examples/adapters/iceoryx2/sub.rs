//! iceoryx2 subscriber example — subscribes to the counter published by
//! `iceoryx2_pub` and demonstrates all three polling modes.
//!
//! A port of the legacy `legacy/wingfoil/examples/iceoryx2/sub.rs` onto the next
//! engine. Pass the mode as a CLI argument (defaults to `spin`):
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example iceoryx2_sub --features iceoryx2 -- spin
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example iceoryx2_sub --features iceoryx2 -- threaded
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example iceoryx2_sub --features iceoryx2 -- signaled
//! ```
//!
//! Modes:
//!   spin     — polls iceoryx2 directly in the graph cycle.
//!              Lowest latency, highest CPU (burns one core).
//!   threaded — polls in a background thread, delivers via the channel layer.
//!              One channel-hop of latency, lower CPU.
//!   signaled — event-driven WaitSet (true blocking).
//!              Lowest CPU, highest latency. Requires the publisher to signal on
//!              a matching Event service (handled automatically by the sink).
//!
//! All modes use IPC (shared memory) by default; the adapter also supports
//! `Iceoryx2ServiceVariant::Local` for in-process testing.

use iceoryx2::prelude::ZeroCopySend;
use log::Level::Info;
use wingfoil::adapters::iceoryx2::{Iceoryx2Mode, Iceoryx2SubOpts, iceoryx2_sub_opts};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

/// Must match the publisher's payload type exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

fn main() -> anyhow::Result<()> {
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

    let service_name = "legacy/wingfoil/examples/counter";
    let opts = Iceoryx2SubOpts {
        mode,
        ..Default::default()
    };

    println!("Subscribing to \"{service_name}\" in {mode:?} mode — waiting for publisher...");

    let g = GraphBuilder::new();
    let _sub = iceoryx2_sub_opts::<Counter>(&g, RunMode::RealTime, service_name, opts)?
        .collapse()
        .inspect(|c: &Counter| println!("received seq={}", c.seq))
        .logged("sub", Info);

    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}
