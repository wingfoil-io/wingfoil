// ZMQ publisher with etcd-based service discovery.
//
// Binds a PUB socket on pub_port and registers its address in etcd under
// service_name. The lease-backed registration disappears from etcd ~30 s after
// a crash, or immediately on clean shutdown.
//
// Start a subscriber with `zmq_etcd_sub`, then run this example.
//
// Prerequisites — start etcd locally with Docker:
//
//   docker run --rm -p 2379:2379 \
//     -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//     -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//     gcr.io/etcd-development/etcd:v3.5.0
//
// Run with:
//   RUST_LOG=info cargo run --example zmq_etcd_pub --features zmq-beta,etcd

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::etcd::EtcdConnection;
use wingfoil::adapters::zmq::{EtcdRegistry, ZeroMqPub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let etcd_endpoint = "http://127.0.0.1:2379";
    let pub_port = 7779u16;
    let service_name = "zmq-etcd-example/quotes";

    let conn = EtcdConnection::new(etcd_endpoint);
    ticker(Duration::from_millis(100))
        .count()
        .logged("pub", Info)
        .zmq_pub(pub_port, (service_name, EtcdRegistry::new(conn)))
        .run(RunMode::RealTime, RunFor::Forever)?;

    Ok(())
}
