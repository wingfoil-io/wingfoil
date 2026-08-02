// ZMQ subscriber with etcd-based service discovery.
//
// Looks up the publisher address from etcd — no hardcoded address required.
// Cross-language compatible — Python pub works too.
//
// Prerequisites — start etcd locally with Docker:
//
//   docker run --rm -p 2379:2379 \
//     -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//     -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//     gcr.io/etcd-development/etcd:v3.5.0
//
// Run publisher and subscriber in separate terminals:
//
//   RUST_LOG=info cargo run --example zmq_etcd_pub --features zmq,etcd
//   RUST_LOG=info cargo run --example zmq_etcd_sub --features zmq,etcd

use log::Level::Info;
use wingfoil::adapters::zmq::{EtcdRegistry, zmq_sub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let (data, _status) = zmq_sub::<Vec<u8>>((
        "zmq-etcd-example/quotes",
        EtcdRegistry::new("http://127.0.0.1:2379"),
    ))?;
    data.map(|burst| {
        burst
            .into_iter()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect::<Vec<_>>()
    })
    .logged("sub", Info)
    .run(RunMode::RealTime, RunFor::Forever)?;

    Ok(())
}
