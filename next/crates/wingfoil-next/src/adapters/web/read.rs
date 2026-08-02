//! `web_sub` — receive values from connected WebSocket clients.
//!
//! Each call to [`web_sub`] registers a bounded mpsc listener on the
//! [`WebServer`] for a topic. When a client sends an envelope on that topic, its
//! payload is decoded with the server's codec and yielded to the graph as a
//! `Burst<T>`.
//!
//! The listener mpsc is bounded and the server uses `try_send` with a
//! drop-newest policy, so a misbehaving browser cannot back-pressure the graph.

use anyhow::Result;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use wingfoil_next::{NanoTime, RunMode};

use super::server::{SUBSCRIBE_MPSC_CAPACITY, WebServer};
use crate::Burst;
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream};

/// Subscribe to frames that clients send on `topic`.
///
/// Returns a source `Stream<Burst<T>>`, decoded with the server's configured
/// codec (bincode or JSON). Frames arriving between graph cycles group into one
/// [`Burst`]; decoding errors abort the run with context.
///
/// The stream never ticks in historical mode — whether the server is a
/// [`start_historical`](super::WebServerBuilder::start_historical) no-op *or* a
/// live server driving a graph in
/// [`RunMode::HistoricalFrom`](wingfoil_next::RunMode::HistoricalFrom). Live browser
/// input has no place in a deterministic replay, and an open listener would
/// block the historical run waiting for frames that never come; so `web_sub`
/// yields an empty, immediately-ending stream. A historical `web_pub` still
/// streams its output to the browser as usual.
///
/// Unlike the live `_sub` sources of the other adapters, `web_sub` therefore
/// does **not** reject `RunMode::HistoricalFrom` at wiring — it is finite in
/// that mode, so there is nothing to deadlock.
///
/// # Errors
///
/// Returns an error at wiring time only if the graph's tokio runtime cannot be
/// created.
pub fn web_sub<T>(
    g: &GraphBuilder,
    server: &WebServer,
    topic: impl Into<String>,
) -> Result<Stream<Burst<T>>>
where
    T: DeserializeOwned + Clone + Default + Send + 'static,
{
    let topic = topic.into();
    let codec = server.codec();
    let historical = server.is_historical_noop();

    // Register the mpsc listener at construction time so frames arriving before
    // the graph starts are buffered up to the mpsc capacity rather than lost.
    // Skipped for a no-op server, which has no live socket.
    let rx_opt = if historical {
        None
    } else {
        let (tx, rx) = mpsc::channel(SUBSCRIBE_MPSC_CAPACITY);
        server.inner.register_sub_sender(&topic, tx);
        Some(rx)
    };

    produce_async(
        g,
        move |params: RunParams| async move {
            Ok(async_stream::stream! {
                // A historical run replays deterministically from its sources;
                // an open client listener would never yield and would block the
                // run from completing, so treat web_sub as an empty source here.
                if matches!(params.run_mode, RunMode::HistoricalFrom(_)) {
                    return;
                }
                let Some(mut rx) = rx_opt else { return; };
                while let Some(payload) = rx.recv().await {
                    match codec.decode::<T>(&payload) {
                        Ok(v) => yield Ok((NanoTime::now(), v)),
                        Err(e) => yield Err(anyhow::anyhow!("web_sub '{topic}': {e}")),
                    }
                }
            })
        },
        None,
    )
}
