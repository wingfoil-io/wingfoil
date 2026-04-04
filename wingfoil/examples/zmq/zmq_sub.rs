// NOTE: ZMQ support is beta. Enable with the `zmq-beta` feature flag.
// Run with: RUST_LOG=info cargo run --example zmq_sub --features zmq-beta
use log::Level::Info;
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

fn main() {
    env_logger::init();
    let address = "tcp://127.0.0.1:5555";
    println!("Starting ZMQ receiver, connecting to {address}...");
    // ZMQ carries raw bytes (Vec<u8>) so the wire format stays language-agnostic.
    // We decode each message back to u64 using the same little-endian convention
    // that the publisher used (n.to_le_bytes() / struct.pack('<Q', n)).
    let (data, status) = zmq_sub::<Vec<u8>>(address).expect("zmq_sub failed");
    let data_node = data
        .map(|burst: Burst<Vec<u8>>| -> Burst<u64> {
            burst
                .into_iter()
                .filter_map(|b| <[u8; 8]>::try_from(b).ok().map(u64::from_le_bytes))
                .collect()
        })
        .logged("received", Info)
        .as_node();
    let status_node = status.logged("status", Info).as_node();
    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Forever,
    )
    .run()
    .unwrap();
}
