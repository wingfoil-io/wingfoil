//! etcd write consumer — streams key-value PUTs upstream to etcd.

use super::{EtcdConnection, EtcdKv};
use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;
use etcd_client::Client;
use futures::StreamExt;
use std::pin::Pin;
use std::rc::Rc;

/// Write a `Burst<EtcdKv>` stream to etcd via PUT.
///
/// Connects once at startup and issues one PUT per [`EtcdKv`] in each burst.
/// Any PUT error terminates the consumer and propagates to the graph.
#[must_use]
pub fn etcd_pub(
    connection: EtcdConnection,
    upstream: &Rc<dyn Stream<Burst<EtcdKv>>>,
) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(
        move |source: Pin<Box<dyn FutStream<Burst<EtcdKv>>>>| async move {
            let mut client = Client::connect(&connection.endpoints, None)
                .await
                .map_err(|e| anyhow::anyhow!("etcd connect failed: {e}"))?;
            let mut source = source;
            while let Some((_time, burst)) = source.next().await {
                for kv in burst {
                    client
                        .put(kv.key, kv.value, None)
                        .await
                        .map_err(|e| anyhow::anyhow!("etcd put failed: {e}"))?;
                }
            }
            Ok(())
        },
    ))
}

/// Extension trait providing a fluent API for writing streams to etcd.
///
/// Implemented for both `Burst<EtcdKv>` (multi-item) and `EtcdKv` (single-item)
/// streams, so burst wrapping is never required in user code.
pub trait EtcdPubOperators {
    /// Write this stream to etcd via PUT.
    #[must_use]
    fn etcd_pub(self: &Rc<Self>, conn: EtcdConnection) -> Rc<dyn Node>;
}

impl EtcdPubOperators for dyn Stream<Burst<EtcdKv>> {
    fn etcd_pub(self: &Rc<Self>, conn: EtcdConnection) -> Rc<dyn Node> {
        etcd_pub(conn, self)
    }
}

impl EtcdPubOperators for dyn Stream<EtcdKv> {
    fn etcd_pub(self: &Rc<Self>, conn: EtcdConnection) -> Rc<dyn Node> {
        let burst_stream = self.map(|kv| {
            let mut b = Burst::new();
            b.push(kv);
            b
        });
        etcd_pub(conn, &burst_stream)
    }
}
