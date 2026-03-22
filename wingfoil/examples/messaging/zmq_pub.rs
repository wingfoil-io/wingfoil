// NOTE: ZMQ support is beta. Enable with the `zmq-beta` feature flag.
// Run with: RUST_LOG=info cargo run --example zmq_pub --features zmq-beta
use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::ZeroMqPub;
use wingfoil::*;

fn main() {
    env_logger::init();
    let port = 5555;
    let period = Duration::from_millis(100);
    ticker(period)
        .count()
        //.limit(10)
        .logged("pub", Info)
        .map(|n: u64| n.to_le_bytes().to_vec())
        .zmq_pub(port)
        .run(RunMode::RealTime, RunFor::Forever)
        .unwrap();
}
