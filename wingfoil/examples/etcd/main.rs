//! etcd adapter example — snapshot + live watch with a round-trip write.
//!
//! Demonstrates:
//! - Seeding keys directly via the etcd client
//! - Using `etcd_sub` to receive the snapshot then live updates
//! - Transforming events and writing back via `etcd_pub`
//!
//! # Prerequisites
//!
//! A running etcd instance:
//! ```sh
//! docker run --rm -p 2379:2379 \
//!   -e ALLOW_NONE_AUTHENTICATION=yes \
//!   bitnami/etcd:3.5
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
    env_logger::init();

    // Seed some source keys using a temporary async runtime.
    // wingfoil.run() must be called from a synchronous context, so async
    // setup is done here before the graph starts.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = etcd_client::Client::connect(&[ENDPOINT], None).await?;
        client
            .put(format!("{SOURCE_PREFIX}greeting"), "hello", None)
            .await?;
        client
            .put(format!("{SOURCE_PREFIX}subject"), "world", None)
            .await?;
        Ok::<_, anyhow::Error>(())
    })?;
    // Drop the runtime before starting the graph (wingfoil creates its own).
    drop(rt);

    println!("Seeded 2 keys under {SOURCE_PREFIX}");
    println!("Watching {SOURCE_PREFIX}, uppercasing values, writing to {DEST_PREFIX}...");

    let conn = EtcdConnection::new(ENDPOINT);

    // Watch the source prefix and uppercase each value, writing to the dest prefix.
    let sub = etcd_sub(conn.clone(), SOURCE_PREFIX);

    let to_write = sub.collapse().map(move |event| {
        let dest_key = event.kv.key.replacen(SOURCE_PREFIX, DEST_PREFIX, 1);
        let upper_value = event
            .kv
            .value_str()
            .unwrap_or("")
            .to_uppercase()
            .into_bytes();
        println!(
            "  {} -> {}",
            event.kv.key,
            String::from_utf8_lossy(&upper_value)
        );
        let mut burst: Burst<EtcdKv> = Burst::new();
        burst.push(EtcdKv {
            key: dest_key,
            value: upper_value,
        });
        burst
    });

    // Process the 2 snapshot events then stop.
    etcd_pub(conn, &to_write).run(RunMode::RealTime, RunFor::Cycles(2))?;

    // Verify the results via a direct read.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = etcd_client::Client::connect(&[ENDPOINT], None).await?;
        let opts = etcd_client::GetOptions::new().with_prefix();
        let resp = client.get(DEST_PREFIX, Some(opts)).await?;
        println!("\nKeys written to {DEST_PREFIX}:");
        for kv in resp.kvs() {
            println!(
                "  {} = {}",
                String::from_utf8_lossy(kv.key()),
                String::from_utf8_lossy(kv.value())
            );
        }
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}
