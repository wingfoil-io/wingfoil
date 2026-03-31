//! etcd write consumer — streams key-value PUTs upstream to etcd.

use super::{EtcdConnection, EtcdEntry};
use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;
use etcd_client::{Client, PutOptions};
use futures::StreamExt;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// Write a `Burst<EtcdEntry>` stream to etcd via PUT.
///
/// Connects once at startup and issues one PUT per [`EtcdEntry`] in each burst.
/// Any PUT error terminates the consumer and propagates to the graph.
///
/// # Leases
///
/// When `lease_ttl` is `Some(duration)`, every key is written under an etcd lease
/// that expires after `duration`. A background keepalive task renews the lease at
/// `ttl/3` intervals so keys remain alive as long as the consumer is running.
/// On clean shutdown the lease is **revoked**, causing all leased keys to vanish
/// immediately rather than waiting for the TTL to expire.
///
/// Use this for presence/heartbeat patterns where keys should disappear as soon as
/// the producer stops.
#[must_use]
pub fn etcd_pub(
    connection: EtcdConnection,
    upstream: &Rc<dyn Stream<Burst<EtcdEntry>>>,
    lease_ttl: Option<Duration>,
) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(
        move |source: Pin<Box<dyn FutStream<Burst<EtcdEntry>>>>| async move {
            let mut client = Client::connect(&connection.endpoints, None)
                .await
                .map_err(|e| anyhow::anyhow!("etcd connect failed: {e}"))?;

            // Optionally grant a lease and start a background keepalive task.
            let (lease_id, keepalive_handle) = match lease_ttl {
                None => (None, None),
                Some(ttl) => {
                    let ttl_secs = ttl.as_secs().max(1) as i64;
                    let lease_resp = client
                        .lease_grant(ttl_secs, None)
                        .await
                        .map_err(|e| anyhow::anyhow!("etcd lease_grant failed: {e}"))?;
                    let id = lease_resp.id();

                    let (mut keeper, mut ka_stream) = client
                        .lease_keep_alive(id)
                        .await
                        .map_err(|e| anyhow::anyhow!("etcd lease_keep_alive failed: {e}"))?;

                    let renew_interval = (ttl / 3).max(Duration::from_secs(1));
                    let handle = tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(renew_interval).await;
                            if keeper.keep_alive().await.is_err() {
                                break;
                            }
                            // Drain the server acknowledgement to keep the stream healthy.
                            match ka_stream.message().await {
                                Ok(Some(_)) => {}
                                _ => break,
                            }
                        }
                    });
                    (Some(id), Some(handle))
                }
            };

            let mut source = source;
            while let Some((_time, burst)) = source.next().await {
                for entry in burst {
                    let opts = lease_id.map(|id| PutOptions::new().with_lease(id));
                    client
                        .put(entry.key, entry.value, opts)
                        .await
                        .map_err(|e| anyhow::anyhow!("etcd put failed: {e}"))?;
                }
            }

            // Stop keepalive and revoke lease so keys expire immediately.
            if let Some(handle) = keepalive_handle {
                handle.abort();
            }
            if let Some(id) = lease_id {
                let _ = client.lease_revoke(id).await;
            }

            Ok(())
        },
    ))
}

/// Extension trait providing a fluent API for writing streams to etcd.
///
/// Implemented for both `Burst<EtcdEntry>` (multi-item) and `EtcdEntry` (single-item)
/// streams, so burst wrapping is never required in user code.
pub trait EtcdPubOperators {
    /// Write this stream to etcd via PUT.
    ///
    /// Pass `lease_ttl: None` for plain writes (keys persist until overwritten or deleted).
    /// Pass `lease_ttl: Some(duration)` to attach a lease with automatic keepalive renewal.
    #[must_use]
    fn etcd_pub(self: &Rc<Self>, conn: EtcdConnection, lease_ttl: Option<Duration>)
    -> Rc<dyn Node>;
}

impl EtcdPubOperators for dyn Stream<Burst<EtcdEntry>> {
    fn etcd_pub(
        self: &Rc<Self>,
        conn: EtcdConnection,
        lease_ttl: Option<Duration>,
    ) -> Rc<dyn Node> {
        etcd_pub(conn, self, lease_ttl)
    }
}

impl EtcdPubOperators for dyn Stream<EtcdEntry> {
    fn etcd_pub(
        self: &Rc<Self>,
        conn: EtcdConnection,
        lease_ttl: Option<Duration>,
    ) -> Rc<dyn Node> {
        let burst_stream = self.map(|kv| {
            let mut b = Burst::new();
            b.push(kv);
            b
        });
        etcd_pub(conn, &burst_stream, lease_ttl)
    }
}
