// NOTE: ZMQ support is beta. Enable with the `zmq-beta` feature flag.
use log::Level::Info;
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

fn main() {
    env_logger::init();
    let address = "tcp://127.0.0.1:5555";
    println!("Starting ZMQ receiver, connecting to {address}...");
    let (data, status) = zmq_sub::<u64>(address);
    let data_node = data.logged("received", Info).as_node();
    let status_node = status.logged("status", Info).as_node();
    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Forever,
    )
    .run()
    .unwrap();
}
