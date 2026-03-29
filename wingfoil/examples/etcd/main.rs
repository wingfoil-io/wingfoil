//! etcd adapter example — watch a key prefix, transform values, write back.
//!
//! # Prerequisites
//!
//! A running etcd instance:
//! ```sh
//! docker run --rm -p 2379:2379 -e ALLOW_NONE_AUTHENTICATION=yes bitnami/etcd:3.5
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

    // Watch source prefix, uppercase each value, write to dest prefix.
    // Two snapshot events → RunFor::Cycles(2).
    // `initially_async` seeds the source keys during graph start.
    etcd_sub(conn.clone(), SOURCE_PREFIX)
        .initially_async(|| async {
            let mut client = etcd_client::Client::connect(&[ENDPOINT], None).await?;
            client
                .put(format!("{SOURCE_PREFIX}greeting"), "hello", None)
                .await?;
            client
                .put(format!("{SOURCE_PREFIX}subject"), "world", None)
                .await?;
            Ok(())
        })
        .collapse()
        .map(|event| {
            let dest_key = event.kv.key.replacen(SOURCE_PREFIX, DEST_PREFIX, 1);
            let upper = event
                .kv
                .value_str()
                .unwrap_or("")
                .to_uppercase()
                .into_bytes();
            println!("  {} → {}", event.kv.key, String::from_utf8_lossy(&upper));
            EtcdKv {
                key: dest_key,
                value: upper,
            }
        })
        .etcd_pub(conn)
        .run(RunMode::RealTime, RunFor::Cycles(2))?;

    Ok(())
}
