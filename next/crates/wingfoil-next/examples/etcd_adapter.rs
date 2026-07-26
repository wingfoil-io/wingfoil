//! etcd adapter example — watch a key prefix, transform values, write back.
//!
//! A port of the classic `wingfoil/examples/etcd` example onto the next engine.
//! Two independent graph roots share one [`GraphBuilder`]:
//!
//! - `seed`       — writes source keys once via `constant` + `etcd_pub`.
//! - `round_trip` — watches the source prefix, uppercases each value, and writes
//!   the result to a destination prefix.
//!
//! The graph owns the tokio runtime the adapters spawn on. This example builds
//! its own runtime and installs it as the override via `with_async_runtime` (so
//! it controls the runtime's lifetime); the simplest usage omits that and lets
//! the graph create one lazily (see the module docs).
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
//! cargo run -p wingfoil-next --example etcd_adapter --features etcd
//! ```

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::etcd::{EtcdConnection, EtcdEntry, EtcdSinkOps, etcd_sub};
use wingfoil_next::async_source::RunParams;
use wingfoil_next::prelude::*;

const ENDPOINT: &str = "http://localhost:2379";
const SOURCE_PREFIX: &str = "/example/source/";
const DEST_PREFIX: &str = "/example/dest/";

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let conn = EtcdConnection::new(ENDPOINT);

    // The graph owns the runtime the adapters spawn on. Here we hand it our own
    // (the override) so this example controls the runtime's lifetime; omit
    // `.with_async_runtime(..)` to let the graph create and own one lazily.
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());

    // Seed the source prefix with two keys, once.
    let _seed = g
        .constant(burst![
            EtcdEntry {
                key: format!("{SOURCE_PREFIX}greeting"),
                value: b"hello".to_vec(),
            },
            EtcdEntry {
                key: format!("{SOURCE_PREFIX}subject"),
                value: b"world".to_vec(),
            },
        ])
        .etcd_pub(conn.clone(), None, true)?;

    // Watch the source prefix, uppercase each value, write to the dest prefix.
    let params = RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Cycles(3),
        start_time: NanoTime::ZERO,
    };
    let _round_trip = etcd_sub(&g, params.run_mode, conn.clone(), SOURCE_PREFIX)?
        .map(|burst: &Burst<_>| {
            burst
                .iter()
                .map(|event: &wingfoil_next::adapters::etcd::EtcdEvent| {
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
        .etcd_pub(conn, None, true)?;

    g.build().run(RunMode::RealTime, RunFor::Cycles(3))?;
    Ok(())
}
