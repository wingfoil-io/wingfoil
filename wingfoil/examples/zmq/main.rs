// ZMQ service discovery example — seed-based.
//
// Demonstrates seed-based discovery: the subscriber never hardcodes the
// publisher address — it queries a seed node at startup.
//
// Run with:
//   RUST_LOG=info cargo run --example zmq --features zmq-beta

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::{SeedRegistry, ZeroMqPub, start_seed, zmq_sub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let seed_addr = "tcp://127.0.0.1:7777";
    let pub_port = 7778u16;
    let run_for = RunFor::Duration(Duration::from_secs(3));

    // Start the seed on a well-known address. Stops when dropped at end of main.
    let _seed = start_seed(seed_addr)?;
    std::thread::sleep(Duration::from_millis(50));

    // Publisher: bind on port 7778, register as "quotes" with the seed.
    // Constructed inside a thread because Rc is !Send.
    std::thread::spawn(move || {
        ticker(Duration::from_millis(100))
            .count()
            .logged("pub", Info)
            .zmq_pub(pub_port, ("quotes", SeedRegistry::new(&[seed_addr])))
            .run(RunMode::RealTime, run_for)
            .expect("publisher failed");
    });

    // Give the publisher time to start and register.
    std::thread::sleep(Duration::from_millis(200));

    // Subscriber: discover "quotes" via the seed — no hardcoded publisher address.
    let (data, _status) = zmq_sub::<u64>(("quotes", SeedRegistry::new(&[seed_addr])))?;
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
