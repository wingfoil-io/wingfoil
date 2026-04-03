// ZMQ service discovery example — etcd-based.
//
// Demonstrates etcd-backed discovery: the publisher writes its address to etcd
// under a lease; the subscriber reads the address from etcd at startup.
// The address vanishes from etcd ~30 s after a crash, or immediately on clean
// shutdown (lease revoke).
//
// Prerequisites — start etcd locally with Docker:
//
//   docker run --rm -p 2379:2379 \
//     -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//     -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//     gcr.io/etcd-development/etcd:v3.5.0
//
// Run with:
//   RUST_LOG=info cargo run --example zmq_etcd_discovery --features zmq-beta,etcd

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::etcd::EtcdConnection;
use wingfoil::adapters::zmq::{EtcdRegistry, ZeroMqPub, zmq_sub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let etcd_endpoint = "http://127.0.0.1:2379";
    let pub_port = 7779u16;
    let service_name = "zmq-etcd-example/quotes";
    let run_for = RunFor::Duration(Duration::from_secs(5));

    // Publisher: bind on pub_port and register address in etcd under service_name.
    // Runs in a thread because Rc is !Send.
    std::thread::spawn(move || {
        let conn = EtcdConnection::new(etcd_endpoint);
        ticker(Duration::from_millis(100))
            .count()
            .logged("pub", Info)
            .zmq_pub(pub_port, (service_name, EtcdRegistry::new(conn)))
            .run(RunMode::RealTime, run_for)
            .expect("publisher failed");
    });

    // Give the publisher time to start and write its address to etcd.
    std::thread::sleep(Duration::from_millis(300));

    // Subscriber: look up the publisher address from etcd — no hardcoded address.
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
