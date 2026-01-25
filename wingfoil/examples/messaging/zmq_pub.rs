use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::ZeroMqPub;
use wingfoil::*;

fn main() {
    env_logger::init();

    let port = 5555;
    let period = Duration::from_millis(100);
    let run_for = RunFor::Duration(Duration::from_secs(10));

    println!("Starting ZMQ sender on port {port}...");
    println!("Publishing an incrementing counter every {period:?}");

    ticker(period)
        .count()
        .logged("pub", Info)
        .zmq_pub(port)
        .run(RunMode::RealTime, run_for)
        .unwrap();

    println!("Finished.");
}
