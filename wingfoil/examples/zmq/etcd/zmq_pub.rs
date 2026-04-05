// ZMQ publisher with etcd-based service discovery.
//
// Publishes a UTF-8 counter string every 100 ms and registers its address in
// etcd under service_name. Cross-language compatible — Python sub works too.
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
//   RUST_LOG=info cargo run --example zmq_etcd_pub --features zmq-beta,etcd
//   RUST_LOG=info cargo run --example zmq_etcd_sub --features zmq-beta,etcd

use log::Level::Info;
use std::time::Duration;
use wingfoil::adapters::zmq::{EtcdRegistry, ZeroMqPub};
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| format!("{n}").into_bytes())
        .logged("pub", Info)
        .zmq_pub(
            7779,
            (
                "zmq-etcd-example/quotes",
                EtcdRegistry::new("http://127.0.0.1:2379"),
            ),
        )
        .run(RunMode::RealTime, RunFor::Forever)?;

    Ok(())
}
