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

/// Read LMAX credentials from env vars; return None (and print a skip notice) if absent.
fn lmax_credentials() -> Option<(String, String)> {
    let user = std::env::var("LMAX_USERNAME").ok()?;
    let pass = std::env::var("LMAX_PASSWORD").ok()?;
    Some((user, pass))
}

/// Verify the FIX session reaches LoggedIn within 10 seconds.
#[test]
fn lmax_logon() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let Some((username, password)) = lmax_credentials() else {
        eprintln!("SKIP lmax_logon: LMAX_USERNAME/LMAX_PASSWORD not set");
        return Ok(());
    };

    let fix = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    let status_node = fix.status.collect().finally(|items, _| {
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
    let Some((username, password)) = lmax_credentials() else {
        eprintln!("SKIP lmax_market_data: LMAX_USERNAME/LMAX_PASSWORD not set");
        return Ok(());
    };

    let fix = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    // Subscribe via graph node — waits for LoggedIn automatically
    let sub = fix.fix_sub(&[AVAX_USD_ID]);

    let data_node = fix.data.collect().finally(|items, _| {
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

    let status_node = fix.status.collect().finally(|items, _| {
        let vs: Vec<FixSessionStatus> = items.into_iter().flat_map(|i| i.value).collect();
        assert!(
            vs.contains(&FixSessionStatus::LoggedIn),
            "Expected LoggedIn, got: {vs:?}"
        );
        Ok(())
    });

    Graph::new(
        vec![data_node, status_node, sub],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(20)),
    )
    .run()
}
