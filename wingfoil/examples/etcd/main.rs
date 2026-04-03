//! etcd adapter example — watch a key prefix, transform values, write back.
//!
//! Two independent graph roots:
//! - `seed`        — writes source keys once via `constant` + `etcd_pub`
//! - `round_trip`  — watches the source prefix, uppercases values, writes to dest prefix
//!
//! # Prerequisites
//!
//! A running etcd instance:
//! ```sh
//! docker run --rm -p 2379:2379 \
//!   -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//!   -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//!   gcr.io/etcd-development/etcd:v3.5.0
//! ```
//!
//! # Run
//!
//! ```sh
//! cargo run --example etcd --features etcd
//! ```

use wingfoil::adapters::etcd::*;
use wingfoil::*;

const ENDPOINT: &str = "http://localhost:2379";
const SOURCE_PREFIX: &str = "/example/source/";
const DEST_PREFIX: &str = "/example/dest/";

fn main() -> anyhow::Result<()> {
    let conn = EtcdConnection::new(ENDPOINT);

    let seed = constant(burst![
        EtcdEntry {
            key: format!("{SOURCE_PREFIX}greeting"),
            value: b"hello".to_vec()
        },
        EtcdEntry {
            key: format!("{SOURCE_PREFIX}subject"),
            value: b"world".to_vec()
        },
    ])
    .etcd_pub(conn.clone(), None, true);

    let round_trip = etcd_sub(conn.clone(), SOURCE_PREFIX)
        .map(|burst| {
            burst
                .into_iter()
                .map(|event| {
                    let dest_key = event.entry.key.replacen(SOURCE_PREFIX, DEST_PREFIX, 1);
                    let upper = event
                        .entry
                        .value_str()
                        .unwrap_or("")
                        .to_uppercase()
                        .into_bytes();
                    println!(
                        "  {} → {}",
                        event.entry.key,
                        String::from_utf8_lossy(&upper)
                    );
                    EtcdEntry {
                        key: dest_key,
                        value: upper,
                    }
                })
                .collect::<Burst<EtcdEntry>>()
        })
        .etcd_pub(conn, None, true);

    Graph::new(vec![seed, round_trip], RunMode::RealTime, RunFor::Cycles(3)).run()?;

    Ok(())
}
