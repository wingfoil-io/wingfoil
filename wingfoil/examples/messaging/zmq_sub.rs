
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

fn main() {
    env_logger::init();

    let address = "tcp://127.0.0.1:5555";
    let run_for = RunFor::Forever;

    println!("Starting ZMQ receiver, connecting to {address}...");

    zmq_sub::<u64>(address)
        .logged("received", log::Level::Info)
        .finally(|values, _| {
            println!("Received {} messages", values.len());
            for v in values {
                println!("  {:?}", v);
            }
        })
        .run(RunMode::RealTime, run_for)
        .unwrap();

    println!("Receiver finished.");
}
