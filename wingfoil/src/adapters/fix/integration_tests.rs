// LMAX London Demo integration tests
//
// Requires a free demo account: https://register.london-demo.lmax.com/registration/LMB/
//
// Run with:
//   LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
//     cargo test --features fix-integration-test -p wingfoil \
//       -- lmax --nocapture --test-threads=1

use super::*;
use crate::{Graph, RunFor, RunMode, StreamOperators};
use std::time::Duration;

const LMAX_MD_HOST: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_MD_PORT: u16 = 443;
const LMAX_MD_TARGET: &str = "LMXBDM";
const AVAX_USD_ID: &str = "100946"; // Avalanche/USD — 24/7 crypto market

/// Read LMAX credentials from env vars; panics if either is missing.
fn lmax_credentials() -> (String, String) {
    let user = std::env::var("LMAX_USERNAME")
        .expect("LMAX_USERNAME env var must be set for integration tests");
    let pass = std::env::var("LMAX_PASSWORD")
        .expect("LMAX_PASSWORD env var must be set for integration tests");
    (user, pass)
}

fn market_data_request() -> FixMessage {
    FixMessage {
        msg_type: "V".to_string(),
        seq_num: 0,
        sending_time: NanoTime::ZERO,
        fields: vec![
            (262, "req1".to_string()),
            (263, "1".to_string()),
            (264, "1".to_string()),
            (265, "0".to_string()),
            (267, "2".to_string()),        // NoMDEntryTypes = 2
            (269, "0".to_string()),        // MDEntryType = Bid
            (269, "1".to_string()),        // MDEntryType = Ask
            (146, "1".to_string()),        // NoRelatedSym = 1
            (48, AVAX_USD_ID.to_string()), // SecurityID = instrument ID (LMAX group delimiter)
            (22, "8".to_string()),         // IDSource = 8 (Exchange Symbol)
        ],
    }
}

/// Verify the FIX session reaches LoggedIn within 10 seconds.
#[test]
fn lmax_logon() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (username, password) = lmax_credentials();

    let (_data, status, _injector) = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    let status_node = status.collect().finally(|items, _| {
        let vs: Vec<FixSessionStatus> = items.into_iter().flat_map(|i| i.value).collect();
        assert!(
            vs.contains(&FixSessionStatus::LoggedIn),
            "Expected LoggedIn, got: {vs:?}"
        );
        Ok(())
    });

    Graph::new(
        vec![status_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(10)),
    )
    .run()
}

/// Verify we can subscribe to AVAX/USD market data and receive at least one snapshot
/// or incremental refresh message (MsgType W or X) within 20 seconds.
#[test]
fn lmax_market_data() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let (username, password) = lmax_credentials();

    let (data, status, injector) = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    // Wait for logon then inject a MarketDataRequest
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        injector.inject(market_data_request());
    });

    let data_node = data.collect().finally(|items, _| {
        let msgs: Vec<FixMessage> = items
            .into_iter()
            .flat_map(|i| i.value.into_iter())
            .collect();
        assert!(!msgs.is_empty(), "No FIX messages received");
        let has_md = msgs.iter().any(|m| m.msg_type == "W" || m.msg_type == "X");
        assert!(
            has_md,
            "Expected MarketDataSnapshotFullRefresh (W) or MarketDataIncrementalRefresh (X), \
             got msg types: {:?}",
            msgs.iter().map(|m| &m.msg_type).collect::<Vec<_>>()
        );
        Ok(())
    });

    let status_node = status.collect().finally(|items, _| {
        let vs: Vec<FixSessionStatus> = items.into_iter().flat_map(|i| i.value).collect();
        assert!(
            vs.contains(&FixSessionStatus::LoggedIn),
            "Expected LoggedIn, got: {vs:?}"
        );
        Ok(())
    });

    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(20)),
    )
    .run()
}
