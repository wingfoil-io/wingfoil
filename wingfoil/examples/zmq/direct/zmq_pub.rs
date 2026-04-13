// ZMQ publisher — direct mode (no service discovery).
//
// Publishes a UTF-8 counter string every 100 ms. The subscriber connects
// by address directly. Cross-language compatible — Python sub works too.
//
// Run publisher and subscriber in separate terminals:
//
//   RUST_LOG=info cargo run --example zmq_direct_pub --features zmq
//   RUST_LOG=info cargo run --example zmq_direct_sub --features zmq

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::ZeroMqPub;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| format!("{n}").into_bytes())
        .logged("pub", Info)
        .zmq_pub(7779, ())
        .run(RunMode::RealTime, RunFor::Forever)?;

    Ok(())
}
