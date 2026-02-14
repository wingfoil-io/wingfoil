use log::Level::Info;
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

fn main() {
    env_logger::init();
    let address = "tcp://127.0.0.1:5555";
    println!("Starting ZMQ receiver, connecting to {address}...");
    zmq_sub::<u64>(address)
        .logged("received", Info)
        .run(RunMode::RealTime, RunFor::Forever)
        .unwrap();
}
