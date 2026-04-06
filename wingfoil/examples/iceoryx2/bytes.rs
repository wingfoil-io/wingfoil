// NOTE: iceoryx2 support is beta. Enable with the `iceoryx2-beta` feature flag.
// Run with: RUST_LOG=info cargo run --example iceoryx2_bytes --features iceoryx2-beta
//
// Demonstrates the byte-slice API using Local (in-process) mode.
// This is useful when payloads are variable-length or when you want to
// serialize data yourself (e.g. JSON, bincode, protobuf).
use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::iceoryx2::{
    Iceoryx2ServiceVariant, Iceoryx2SubOpts, iceoryx2_pub_slice_with, iceoryx2_sub_slice_opts,
};
use wingfoil::*;

fn main() {
    env_logger::init();

    let service_name = "wingfoil/examples/bytes";
    let period = Duration::from_millis(100);

    // --- Publisher side: produce JSON-encoded messages as byte slices ---
    let upstream = ticker(period)
        .count()
        .map(|seq: u64| {
            let json = format!(r#"{{"seq":{seq}}}"#);
            let mut b = Burst::default();
            b.push(json.into_bytes());
            b
        })
        .logged("pub", Info);

    let pub_node = iceoryx2_pub_slice_with(upstream, service_name, Iceoryx2ServiceVariant::Local);

    // --- Subscriber side: receive and decode byte slices ---
    let opts = Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        ..Default::default()
    };
    let sub = iceoryx2_sub_slice_opts(service_name, opts);
    let sub_node = sub
        .collapse()
        .inspect(|bytes: &Vec<u8>| {
            let msg = String::from_utf8_lossy(bytes);
            println!("received: {msg}");
        })
        .logged("sub", Info)
        .as_node();

    println!("Running local byte-slice pub/sub on \"{service_name}\" — press Ctrl-C to stop");
    Graph::new(
        vec![pub_node, sub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(3)),
    )
    .run()
    .unwrap();
}
