// ZMQ service discovery example.
// Run with: RUST_LOG=info cargo run --example zmq_discover --features zmq-beta
//
// Starts a seed, a named publisher, and a discovering subscriber — all in the
// same process. In a real deployment each would run in a separate process;
// only the seed address needs to be known in advance.

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::{ZeroMqPub, zmq_sub_discover};
use wingfoil::adapters::zmq_seed::start_seed;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let seed_addr = "tcp://127.0.0.1:7777";
    let pub_port = 7778u16;
    let run_for = RunFor::Duration(Duration::from_secs(3));

    // Start the seed (stops when dropped at end of main).
    let _seed = start_seed(seed_addr)?;
    std::thread::sleep(Duration::from_millis(50));

    // Publisher: bind on port 7778, register as "quotes" with the seed.
    // Node must be constructed inside the thread (Rc is !Send).
    std::thread::spawn(move || {
        ticker(Duration::from_millis(100))
            .count()
            .logged("pub", Info)
            .zmq_pub_named("quotes", pub_port, &[seed_addr])
            .run(RunMode::RealTime, run_for)
            .expect("publisher failed");
    });

    // Give the publisher time to start and register.
    std::thread::sleep(Duration::from_millis(200));

    // Subscriber: discover "quotes" via the seed, no hardcoded pub address.
    let (data, _status) = zmq_sub_discover::<u64>("quotes", &[seed_addr])?;
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
