// ZMQ subscriber with etcd-based service discovery.
//
// Looks up the publisher address from etcd under service_name — no hardcoded
// address required. Start `zmq_etcd_pub` first so the address is registered.
//
// Prerequisites — start etcd locally with Docker:
//
//   docker run --rm -p 2379:2379 \
//     -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//     -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//     gcr.io/etcd-development/etcd:v3.5.0
//
// Run with:
//   RUST_LOG=info cargo run --example zmq_etcd_sub --features zmq-beta,etcd

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::etcd::EtcdConnection;
use wingfoil::adapters::zmq::{EtcdRegistry, zmq_sub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let etcd_endpoint = "http://127.0.0.1:2379";
    let service_name = "zmq-etcd-example/quotes";
    let run_for = RunFor::Duration(Duration::from_secs(5));

    let conn = EtcdConnection::new(etcd_endpoint);
    let (data, _status) = zmq_sub::<u64>((service_name, EtcdRegistry::new(conn)))?;
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
