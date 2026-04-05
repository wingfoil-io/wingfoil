// ZMQ subscriber — direct mode (no service discovery).
//
// Connects to the publisher address directly. Start `zmq_direct_pub` first.
//
// Run publisher and subscriber in separate terminals:
//
//   RUST_LOG=info cargo run --example zmq_direct_pub --features zmq-beta
//   RUST_LOG=info cargo run --example zmq_direct_sub --features zmq-beta

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let pub_address = "tcp://127.0.0.1:7779";
    let run_for = RunFor::Duration(Duration::from_secs(5));

    let (data, _status) = zmq_sub::<u64>(pub_address)?;
    data.logged("sub", Info)
        .collect()
        .finally(|res, _| {
            let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
            println!("received {} values: {:?}", values.len(), values);
            Ok(())
        })
        .run(RunMode::RealTime, run_for)?;

    Ok(())
}
