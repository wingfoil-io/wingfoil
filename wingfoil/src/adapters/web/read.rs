//! `web_sub` — receive values from connected WebSocket clients.
//!
//! Each call to [`web_sub`] registers an mpsc listener on the
//! [`WebServer`] for a topic. When a client sends an envelope on that
//! topic, its payload is decoded with the server's codec and yielded to
//! the graph as a `Burst<T>`.
//!
//! The listener mpsc is bounded, and the server uses `try_send` with a
//! drop-newest policy so a misbehaving browser cannot back-pressure the
//! graph.

use std::rc::Rc;

use serde::de::DeserializeOwned;

use super::server::{WebServer, subscribe_channel};
use crate::nodes::{RunParams, produce_async};
use crate::types::*;

/// Subscribe to frames that clients send on `topic`.
///
/// Returns a source [`Stream`] that emits `Burst<T>` values. The payload
/// is deserialized with the server's configured codec (bincode or
/// JSON). Decoding errors are surfaced as graph errors via the stream.
///
/// In historical mode ([`WebServer::is_historical_noop`]) no listener
/// is registered and the stream never ticks.
#[must_use]
pub fn web_sub<T: Element + Send + DeserializeOwned>(
    server: &WebServer,
    topic: impl Into<String>,
) -> Rc<dyn Stream<Burst<T>>> {
    let topic = topic.into();
    let codec = server.codec();
    let historical = server.is_historical_noop();

    // Register the mpsc listener at construction time so frames
    // arriving before the graph starts are buffered up to the mpsc
    // capacity instead of being dropped.
    let rx_opt = if historical {
        None
    } else {
        let (tx, rx) = subscribe_channel();
        server.inner.register_sub_sender(&topic, tx);
        Some(rx)
    };

    produce_async(move |_ctx: RunParams| async move {
        Ok(async_stream::stream! {
            let Some(mut rx) = rx_opt else {
                // Historical no-op: produce no items.
                return;
            };
            while let Some(payload) = rx.recv().await {
                match codec.decode_payload::<T>(&payload) {
                    Ok(v) => yield Ok((NanoTime::now(), v)),
                    Err(e) => yield Err(anyhow::anyhow!("web_sub decode on '{topic}': {e}")),
                }
            }
        })
    })
}
