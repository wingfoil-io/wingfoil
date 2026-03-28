// FIX protocol client example — connects to a FIX acceptor and prints incoming messages.
//
// Run with:
//   RUST_LOG=info cargo run --example fix_client --features fix
//
// Start a counterparty first (e.g. a FIX engine listening on port 9999).
use log::Level::Info;
use wingfoil::adapters::fix::{FixPollMode, fix_connect};
use wingfoil::*;

fn main() {
    env_logger::init();

    let (data, status) = fix_connect("127.0.0.1", 9999, "CLIENT", "SERVER", FixPollMode::Threaded);

    let data_node = data.logged("fix", Info).as_node();
    let status_node = status.logged("fix-status", Info).as_node();

    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Forever,
    )
    .run()
    .unwrap();
}
