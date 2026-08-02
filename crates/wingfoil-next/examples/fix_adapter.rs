//! FIX loopback — runs an acceptor and an initiator in the same process on the
//! next engine. A self-contained port of the classic
//! `wingfoil/examples/fix/fix_loopback`.
//!
//! Demonstrates:
//!   - [`fix_accept`](wingfoil_next::adapters::fix::fix_accept) (acceptor side)
//!   - [`fix_connect`](wingfoil_next::adapters::fix::fix_connect) with the
//!     `AlwaysSpin` poll mode (lowest-latency, graph-driven)
//!   - downstream stream operators (`map`/`fold`/`map_filter`/`logged`) on the
//!     FIX session-status and data streams
//!
//! No external FIX engine is required — the example is entirely self-contained.
//! The acceptor is wired **first** so its listener binds (at graph `start()`)
//! before the spin initiator's synchronous connect runs.
//!
//! Of classic's five `wingfoil/examples/fix/` programs, only `fix_loopback` is
//! self-contained, so it is the one ported here. The other four are
//! peer/credential-dependent and are **not** ported: `fix_client` /
//! `fix_echo_server` each demonstrate one half of a connection and need a
//! separate counterparty process, and `lmax_demo` / `lmax_instruments` require a
//! (free) LMAX London Demo account and credentials. All the API they exercise
//! (`fix_connect`, `fix_accept`, `fix_connect_tls`, `fix_sub`, `fix_send`) is
//! ported and covered by the adapter tests.
//!
//! # Run
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil-next --example fix_adapter --features fix
//! ```

use std::time::Duration;

use log::Level::Info;
use log::info;
use wingfoil_next::adapters::fix::{FixPollMode, FixSessionStatus, fix_accept, fix_connect};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let port = 19876;
    info!("Starting FIX loopback on port {port}");
    let g = GraphBuilder::new();

    // ── Acceptor side (wired first so its listener is bound before connect) ──
    let (acc_data, acc_status) = fix_accept(
        &g,
        RunMode::RealTime,
        port,
        "ACCEPTOR",
        "INITIATOR",
        FixPollMode::AlwaysSpin,
    )?;

    // ── Initiator side ──────────────────────────────────────────────────────
    let (init_data, init_status) = fix_connect(
        &g,
        RunMode::RealTime,
        "127.0.0.1",
        port,
        "INITIATOR",
        "ACCEPTOR",
        FixPollMode::AlwaysSpin,
    )?;

    // ── Downstream operators ────────────────────────────────────────────────

    // Running count of FIX data messages received by the acceptor.
    let _acc_msg_count = acc_data
        .map(|burst: &_| burst.len())
        .fold(0usize, |acc, n| *acc += n)
        .logged("acceptor-msg-count", Info);

    // Initiator status filtered to LoggedIn transitions only.
    let _init_logged_in = init_status
        .map_filter(|burst: &_| {
            let logged_in = burst.contains(&FixSessionStatus::LoggedIn);
            (logged_in, logged_in)
        })
        .logged("initiator-logon", Info);

    // Log acceptor status events, and keep the initiator data stream live.
    let _acc_status = acc_status.logged("acceptor-status", Info);
    let _init_data = init_data.map(|burst: &_| burst.len());

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))?;

    info!("Done.");
    Ok(())
}
