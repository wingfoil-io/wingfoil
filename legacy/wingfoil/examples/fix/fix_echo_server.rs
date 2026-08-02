// FIX protocol acceptor example — binds a port and logs incoming FIX messages.
//
// Run with:
//   RUST_LOG=info cargo run --example fix_echo_server --features fix
//
// Then connect a FIX initiator (e.g. the fix_client example) to port 9999.

use log::Level::Info;
use wingfoil::adapters::fix::{FixPollMode, fix_accept};
use wingfoil::*;

fn main() {
    env_logger::init();

    let port = 9999;
    println!("Listening for FIX initiator on port {port}...");

    let (data, status) = fix_accept(port, "SERVER", "CLIENT", FixPollMode::Threaded);

    let data_node = data.logged("fix-data", Info).as_node();
    let status_node = status.logged("fix-status", Info).as_node();

    Graph::new(
        vec![data_node, status_node],
        RunMode::RealTime,
        RunFor::Forever,
    )
    .run()
    .unwrap();
}
