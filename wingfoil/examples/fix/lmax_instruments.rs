// LMAX London Demo — discover available instruments via SecurityListRequest
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

fn security_list_request() -> FixMessage {
    FixMessage {
        msg_type: "x".to_string(),
        seq_num: 0,
        sending_time: NanoTime::ZERO,
        fields: vec![
            (320, "list1".to_string()), // SecurityReqID
            (321, "0".to_string()),     // SecurityListRequestType = 0 (all securities)
            (55, "[N/A]".to_string()),  // Symbol (required placeholder)
        ],
    }
}

fn print_security_list(msg: &FixMessage) {
    let get = |tag: u32| -> &str {
        msg.fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v): &(u32, String)| v.as_str())
            .unwrap_or("-")
    };
    println!(
        "SecurityList: TotNoRelatedSym={} ReqResult={}",
        get(393),
        get(560)
    );
    // Walk repeating group entries: each starts with Symbol (55)
    let fields = &msg.fields;
    let mut i = 0;
    while i < fields.len() {
        if fields[i].0 == 55 {
            let symbol = &fields[i].1;
            let mut security_id = "-";
            let mut sec_type = "-";
            let mut currency = "-";
            let mut j = i + 1;
            while j < fields.len() && fields[j].0 != 55 {
                match fields[j].0 {
                    48 => security_id = &fields[j].1,
                    167 => sec_type = &fields[j].1,
                    15 => currency = &fields[j].1,
                    _ => {}
                }
                j += 1;
            }
            println!(
                "  {:>8}  {:<30}  type={:<8}  ccy={}",
                security_id, symbol, sec_type, currency
            );
            i = j;
        } else {
            i += 1;
        }
    }
}

fn main() {
    let username =
        std::env::var("LMAX_USERNAME").expect("LMAX_USERNAME environment variable not set");
    let password =
        std::env::var("LMAX_PASSWORD").expect("LMAX_PASSWORD environment variable not set");

    let (data, status, injector) = fix_connect_tls(
        LMAX_MD_HOST,
        LMAX_MD_PORT,
        &username,
        LMAX_MD_TARGET,
        Some(&password),
    );

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        injector.inject(security_list_request());
    });

    let data_node = data
        .map(|burst| {
            for msg in &burst {
                match msg.msg_type.as_str() {
                    "y" => print_security_list(msg),
                    "3" => {
                        let text = msg
                            .fields
                            .iter()
                            .find(|f| f.0 == 58)
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("(no text)");
                        eprintln!("Reject: {text}");
                    }
                    _ => {}
                }
            }
            burst
        })
        .as_node();

    let status_node = status
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
        RunFor::Duration(Duration::from_secs(15)),
    )
    .run()
    .unwrap();
}
