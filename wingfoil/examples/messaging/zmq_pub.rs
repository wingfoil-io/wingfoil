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
        .zmq_pub(port)
        .run(RunMode::RealTime, RunFor::Forever)
        .unwrap();
}
