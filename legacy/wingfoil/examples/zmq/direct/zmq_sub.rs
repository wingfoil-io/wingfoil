// ZMQ subscriber — direct mode (no service discovery).
//
// Connects to the publisher address directly. Start `zmq_direct_pub` first.
// Cross-language compatible — Python pub works too.
//
// Run publisher and subscriber in separate terminals:
//
//   RUST_LOG=info cargo run --example zmq_direct_pub --features zmq
//   RUST_LOG=info cargo run --example zmq_direct_sub --features zmq

use log::Level::Info;
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let (data, _status) = zmq_sub::<Vec<u8>>("tcp://127.0.0.1:7779")?;
    data.map(|burst| {
        burst
            .into_iter()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect::<Vec<_>>()
    })
    .logged("sub", Info)
    .run(RunMode::RealTime, RunFor::Forever)?;

    Ok(())
}
