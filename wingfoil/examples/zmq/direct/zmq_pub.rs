// ZMQ publisher — direct mode (no service discovery).
//
// Binds a PUB socket on pub_port and publishes a counter every 100 ms.
// The subscriber connects to the address directly.
//
// Run publisher and subscriber in separate terminals:
//
//   RUST_LOG=info cargo run --example zmq_direct_pub --features zmq-beta
//   RUST_LOG=info cargo run --example zmq_direct_sub --features zmq-beta

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::ZeroMqPub;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let pub_port = 7779u16;

    ticker(Duration::from_millis(100))
        .count()
        .logged("pub", Info)
        .zmq_pub(pub_port, ())
        .run(RunMode::RealTime, RunFor::Forever)?;

    Ok(())
}
