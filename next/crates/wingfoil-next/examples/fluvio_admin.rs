//! Fluvio cluster admin calls, for bootstrapping a cluster from a shell.
//!
//! `tests/fluvio_integration.rs` brings up its own cluster from Rust, because
//! the two calls needed — registering the SPU with the SC before the SPU
//! process connects, and creating a topic — are `FluvioAdmin` operations with
//! no shell equivalent available to us:
//!
//! - the `infinyon/fluvio` image ships only `/fluvio-run` (the server), not the
//!   `fluvio` CLI;
//! - and CI has no network route to `hub.infinyon.cloud` to install one.
//!
//! The Python integration leg needs the same bootstrap but drives it from bash,
//! so this exposes those two calls as a tiny command. It is the same code the
//! test harness runs, rather than a second implementation of it.
//!
//! ```bash
//! cargo run --features fluvio-integration-test --example fluvio_admin -- \
//!     register-spu 127.0.0.1:9003
//! cargo run --features fluvio-integration-test --example fluvio_admin -- \
//!     create-topic 127.0.0.1:9003 my-topic
//! ```
//!
//! Both calls are **idempotent**: an SPU or topic that already exists is
//! success, so re-running against a live cluster is safe.

use anyhow::{Result, anyhow, bail};
use fluvio::FluvioAdmin;
use fluvio::FluvioClusterConfig;
use fluvio::metadata::customspu::CustomSpuSpec;
use fluvio::metadata::topic::TopicSpec;
use fluvio_controlplane_metadata::spu::{EncryptionEnum, Endpoint, IngressPort};

/// The SPU identity the integration harness pins (see `fluvio_integration.rs`:
/// host networking fixes these ports, so the id and endpoints must match).
const SPU_ID: i32 = 5001;
const SPU_PORT: u16 = 9010;
const SPU_PRIVATE_PORT: u16 = 9011;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Runtime::new()?;
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["register-spu", endpoint] => rt.block_on(register_spu(endpoint)),
        ["create-topic", endpoint, topic] => rt.block_on(create_topic(endpoint, topic)),
        _ => bail!(
            "usage: fluvio_admin register-spu <sc-addr>\n       \
             fluvio_admin create-topic <sc-addr> <topic>"
        ),
    }
}

async fn admin(endpoint: &str) -> Result<FluvioAdmin> {
    FluvioAdmin::connect_with_config(&FluvioClusterConfig::new(endpoint))
        .await
        .map_err(|e| anyhow!("fluvio admin connect to {endpoint} failed: {e}"))
}

/// Register the custom SPU with the SC. This must happen **before** the SPU
/// process connects, or the SC closes its connection.
async fn register_spu(endpoint: &str) -> Result<()> {
    let spec = CustomSpuSpec {
        id: SPU_ID,
        public_endpoint: IngressPort::from_port_host(SPU_PORT, "127.0.0.1".to_string()),
        private_endpoint: Endpoint {
            port: SPU_PRIVATE_PORT,
            host: "127.0.0.1".to_string(),
            encryption: EncryptionEnum::PLAINTEXT,
        },
        rack: None,
        public_endpoint_local: None,
    };
    match admin(endpoint)
        .await?
        .create::<CustomSpuSpec>(format!("spu-{SPU_ID}"), false, spec)
        .await
    {
        Ok(()) => Ok(()),
        Err(e) if is_already_exists(&e) => Ok(()),
        Err(e) => Err(anyhow!("SPU register failed: {e:?}")),
    }
}

async fn create_topic(endpoint: &str, topic: &str) -> Result<()> {
    match admin(endpoint)
        .await?
        .create::<TopicSpec>(
            topic.to_string(),
            false,
            TopicSpec::new_computed(1, 1, None),
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(e) if is_already_exists(&e) => Ok(()),
        Err(e) => Err(anyhow!("topic create failed: {e}")),
    }
}

/// Treat an "already there" rejection as success, so the command is idempotent.
fn is_already_exists<E: std::fmt::Debug>(err: &E) -> bool {
    let rendered = format!("{err:?}").to_lowercase();
    rendered.contains("already defined") || rendered.contains("already exists")
}
