// LMAX London Demo — FIX 4.4 market data subscription via TLS
//
// Prerequisites:
//   1. Register a free demo account at:
//      https://register.london-demo.lmax.com/registration/LMB/
//   2. Set environment variables:
//      LMAX_USERNAME=<your username>
//      LMAX_PASSWORD=<your password>
//
// Run with:
//   LMAX_USERNAME=xxx LMAX_PASSWORD=yyy RUST_LOG=info \
//     cargo run --example lmax_demo --features fix
//
// Session details:
//   Host:          fix-marketdata.london-demo.lmax.com:443
//   TargetCompID:  LMXBDM
//   SenderCompID:  <your LMAX username>
//   Protocol:      FIX 4.4 over TLS
//
// The example connects to the market data session, logs on, then subscribes to
// EUR/USD (LMAX instrument ID 4001). Incoming MarketDataSnapshotFullRefresh and
// MarketDataIncrementalRefresh messages are printed for 60 seconds.
//
// To use the order routing session instead, change the host/port/target:
//   Host:          fix-order.london-demo.lmax.com:443
//   TargetCompID:  LMXBD

use std::time::Duration;

use log::Level::Info;
use log::info;
use wingfoil::adapters::fix::fix_connect_tls;
use wingfoil::*;

// ── LMAX London Demo connection parameters ────────────────────────────────────

const LMAX_MD_HOST: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_MD_PORT: u16 = 443;
const LMAX_MD_TARGET: &str = "LMXBDM";

// LMAX instrument IDs (London Demo — verify against the LMAX instrument list at
// https://docs.lmax.com or your account portal before use).
const EUR_USD_ID: &str = "4001";

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    env_logger::init();

    let username =
        std::env::var("LMAX_USERNAME").expect("LMAX_USERNAME environment variable not set");
    let password =
        std::env::var("LMAX_PASSWORD").expect("LMAX_PASSWORD environment variable not set");

    info!("Connecting to LMAX London Demo market data session at {LMAX_MD_HOST}:{LMAX_MD_PORT}");
    info!("  SenderCompID = {username}  TargetCompID = {LMAX_MD_TARGET}");

    let fix = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    // Subscribe to EUR/USD — the node waits for LoggedIn then sends the request.
    let sub = fix.fix_sub(constant(vec![EUR_USD_ID.into()]));

    let data_node = fix.data.logged("fix-data", Info).as_node();
    let status_node = fix.status.logged("fix-status", Info).as_node();

    Graph::new(
        vec![data_node, status_node, sub],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(60)),
    )
    .run()
    .unwrap();

    info!("Done.");
}
