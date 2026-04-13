// LMAX London Demo — probe instrument IDs by subscribing and observing the response.
//
// Run with:
//   LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
//     cargo run --example lmax_instruments --features fix

use std::time::Duration;

use wingfoil::adapters::fix::{FixMessage, fix_connect_tls};
use wingfoil::*;

const LMAX_MD_HOST: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_MD_PORT: u16 = 443;
const LMAX_MD_TARGET: &str = "LMXBDM";

// Sweep a range of IDs — LMAX crypto instruments are in the 100xxx range
fn candidates() -> Vec<String> {
    (100900..=100999).map(|i| i.to_string()).collect()
}

fn market_data_request(instrument_id: &str, req_id: &str) -> FixMessage {
    FixMessage {
        msg_type: "V".to_string(),
        seq_num: 0,
        sending_time: NanoTime::ZERO,
        fields: vec![
            (262, req_id.to_string()),       // MDReqID
            (263, "1".to_string()),          // SubscriptionRequestType = Subscribe
            (264, "1".to_string()),          // MarketDepth = top of book
            (265, "0".to_string()),          // MDUpdateType = Full Refresh
            (267, "2".to_string()),          // NoMDEntryTypes = 2
            (269, "0".to_string()),          // MDEntryType = Bid
            (269, "1".to_string()),          // MDEntryType = Ask
            (146, "1".to_string()),          // NoRelatedSym = 1
            (48, instrument_id.to_string()), // SecurityID (LMAX group delimiter)
            (22, "8".to_string()),           // IDSource = Exchange Symbol
        ],
    }
}

fn main() {
    let username =
        std::env::var("LMAX_USERNAME").expect("LMAX_USERNAME environment variable not set");
    let password =
        std::env::var("LMAX_PASSWORD").expect("LMAX_PASSWORD environment variable not set");

    let fix = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    // Sweep instrument IDs with a delay between each request.
    // This needs raw injection because of the per-request throttling.
    let injector = fix.injector();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        for id in candidates() {
            injector.inject(market_data_request(&id, &format!("req_{id}")));
            std::thread::sleep(Duration::from_millis(100));
        }
    });

    let data_node = fix
        .data
        .map(|burst| {
            for msg in &burst {
                match msg.msg_type.as_str() {
                    "W" | "X" => {
                        let sec_id = msg
                            .fields
                            .iter()
                            .find(|f| f.0 == 48)
                            .map(|f| f.1.as_str())
                            .unwrap_or("-");
                        let bid = msg
                            .fields
                            .iter()
                            .find(|f| f.0 == 270)
                            .map(|f| f.1.as_str())
                            .unwrap_or("-");
                        let ask = msg
                            .fields
                            .iter()
                            .find(|f| f.0 == 271)
                            .map(|f| f.1.as_str())
                            .unwrap_or("-");
                        println!("FOUND  id={sec_id:<8}  bid={bid:<12}  ask={ask}");
                    }
                    // Silently ignore rejects — most IDs won't exist
                    _ => {}
                }
            }
            burst
        })
        .as_node();

    let status_node = fix
        .status
        .map(|burst| {
            for s in &burst {
                eprintln!("status: {s:?}");
            }
            burst
        })
        .as_node();

    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(60)),
    )
    .run()
    .unwrap();
}
