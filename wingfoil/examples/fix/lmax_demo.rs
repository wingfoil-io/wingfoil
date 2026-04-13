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
use wingfoil::adapters::fix::{FixInjector, FixMessage, fix_connect_tls};
use wingfoil::*;

// ── LMAX London Demo connection parameters ────────────────────────────────────

const LMAX_MD_HOST: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_MD_PORT: u16 = 443;
const LMAX_MD_TARGET: &str = "LMXBDM";

// LMAX instrument IDs (London Demo — verify against the LMAX instrument list at
// https://docs.lmax.com or your account portal before use).
const EUR_USD_ID: &str = "4001";

// ── Market data request builder ───────────────────────────────────────────────

/// Build a FIX MarketDataRequest (MsgType V) subscribing to top-of-book EUR/USD.
fn market_data_request(instrument_id: &str) -> FixMessage {
    FixMessage {
        msg_type: "V".to_string(),
        seq_num: 0, // sequence number is set by the session layer
        sending_time: NanoTime::ZERO,
        fields: vec![
            (262, "req1".to_string()),       // MDReqID — unique request identifier
            (263, "1".to_string()),          // SubscriptionRequestType = 1 (Subscribe)
            (264, "1".to_string()),          // MarketDepth = 1 (top of book)
            (265, "0".to_string()),          // MDUpdateType = 0 (Full Refresh)
            (267, "2".to_string()),          // NoMDEntryTypes = 2
            (269, "0".to_string()),          // MDEntryType = 0 (Bid)
            (269, "1".to_string()),          // MDEntryType = 1 (Ask)
            (146, "1".to_string()),          // NoRelatedSym = 1
            (48, instrument_id.to_string()), // SecurityID — LMAX instrument ID (group delimiter)
            (22, "8".to_string()),           // IDSource = 8 (Exchange Symbol)
        ],
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    env_logger::init();

    let username =
        std::env::var("LMAX_USERNAME").expect("LMAX_USERNAME environment variable not set");
    let password =
        std::env::var("LMAX_PASSWORD").expect("LMAX_PASSWORD environment variable not set");

    info!("Connecting to LMAX London Demo market data session at {LMAX_MD_HOST}:{LMAX_MD_PORT}");
    info!("  SenderCompID = {username}  TargetCompID = {LMAX_MD_TARGET}");

    let (data, status, injector) = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    // After a brief delay for the TLS handshake and FIX logon to complete,
    // send a MarketDataRequest for EUR/USD.
    subscribe_after_logon(injector, EUR_USD_ID, Duration::from_secs(3));

    let data_node = data.logged("fix-data", Info).as_node();
    let status_node = status.logged("fix-status", Info).as_node();

    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(60)),
    )
    .run()
    .unwrap();

    info!("Done.");
}

/// Spawn a background thread that waits `delay` then sends a market data subscription.
fn subscribe_after_logon(injector: FixInjector, instrument_id: &str, delay: Duration) {
    let req = market_data_request(instrument_id);
    let id = instrument_id.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        info!("Sending MarketDataRequest (instrument {id})");
        injector.inject(req);
    });
}
