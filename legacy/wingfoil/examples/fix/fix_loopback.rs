// FIX loopback — runs an acceptor and initiator in the same process.
//
// Demonstrates:
//   - `fix_accept` (acceptor side)
//   - `fix_connect` with `AlwaysSpin` poll mode (lowest-latency, graph-driven)
//   - Downstream stream operators (filter, fold) on FIX session status
//
// No external FIX engine is required — the example is entirely self-contained.
//
// Run with:
//   RUST_LOG=info cargo run --example fix_loopback --features fix

use std::time::Duration;

use log::Level::Info;
use log::info;
use wingfoil::adapters::fix::{FixPollMode, FixSessionStatus, fix_accept, fix_connect};
use wingfoil::*;

fn main() {
    env_logger::init();

    // Bind the acceptor on an ephemeral port.
    let port = 19876;
    info!("Starting FIX loopback on port {port}");

    // ── Acceptor side ────────────────────────────────────────────────────────
    let (acc_data, acc_status) = fix_accept(port, "ACCEPTOR", "INITIATOR", FixPollMode::AlwaysSpin);

    // ── Initiator side ───────────────────────────────────────────────────────
    let (init_data, init_status) = fix_connect(
        "127.0.0.1",
        port,
        "INITIATOR",
        "ACCEPTOR",
        FixPollMode::AlwaysSpin,
    );

    // ── Downstream operators ─────────────────────────────────────────────────

    // Count FIX data messages received by the acceptor.
    let acc_msg_count = acc_data
        .map(|burst| burst.len())
        .fold::<usize>(|acc, n| *acc += n);

    // Filter initiator status to LoggedIn events only.
    let init_logged_in = init_status
        .filter_value(|burst| burst.contains(&FixSessionStatus::LoggedIn))
        .logged("initiator-logon", Info);

    Graph::builder()
        .add(init_data)
        .add(init_logged_in)
        // Log acceptor status events.
        .add(acc_status.logged("acceptor-status", Info))
        // Print the running message count.
        .add(acc_msg_count.logged("acceptor-msg-count", Info))
        .real_time()
        .duration(Duration::from_secs(5))
        .run()
        .unwrap();

    info!("Done.");
}
