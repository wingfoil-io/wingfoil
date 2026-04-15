//! Fluvio producer consumer — writes records from a stream to a Fluvio topic.

use super::{FluvioConnection, FluvioRecord};
use crate::RunParams;
use crate::burst;
use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;
use fluvio::{Fluvio, FluvioClusterConfig, RecordKey};
use futures::StreamExt;
use std::pin::Pin;
use std::rc::Rc;

/// Write a `Burst<FluvioRecord>` stream to a Fluvio topic.
///
/// Connects to the Fluvio SC once at startup and issues one `send` per
/// [`FluvioRecord`] in each burst. After each burst all records are flushed
/// to the broker to ensure delivery before the next graph cycle begins.
///
/// Records with `key: None` are sent using [`RecordKey::NULL`].
///
/// # Arguments
///
/// - `connection` — Fluvio cluster configuration (SC endpoint).
/// - `topic` — The Fluvio topic to produce to. The topic must already exist.
/// - `upstream` — The stream of `Burst<FluvioRecord>` to consume.
///
/// # Errors
///
/// The graph stops and propagates an error if:
/// - The connection to the Fluvio SC fails.
/// - The topic does not exist or the producer cannot be created.
/// - Any `send` or `flush` call returns an error.
#[must_use]
pub fn fluvio_pub(
    connection: FluvioConnection,
    topic: impl Into<String>,
    upstream: &Rc<dyn Stream<Burst<FluvioRecord>>>,
) -> Rc<dyn Node> {
    let topic = topic.into();
    upstream.consume_async(Box::new(
        move |_ctx: RunParams, source: Pin<Box<dyn FutStream<Burst<FluvioRecord>>>>| async move {
            let cluster_config = FluvioClusterConfig::new(&connection.endpoint);
            let client = Fluvio::connect_with_config(&cluster_config)
                .await
                .map_err(|e| anyhow::anyhow!("fluvio connect failed: {e}"))?;

            let producer = client
                .topic_producer(&topic)
                .await
                .map_err(|e| anyhow::anyhow!("fluvio producer create failed: {e}"))?;

            let mut source = source;
            while let Some((_time, burst)) = source.next().await {
                for record in burst {
                    let key = record.key.map(RecordKey::from).unwrap_or(RecordKey::NULL);
                    producer
                        .send(key, record.value)
                        .await
                        .map_err(|e| anyhow::anyhow!("fluvio send failed: {e}"))?;
                }
                // Flush once per burst to batch records within a tick for throughput
                // while still guaranteeing delivery before the next cycle.
                producer
                    .flush()
                    .await
                    .map_err(|e| anyhow::anyhow!("fluvio flush failed: {e}"))?;
            }

            Ok(())
        },
    ))
}

/// Extension trait providing a fluent API for writing streams to Fluvio.
///
/// Implemented for both `Burst<FluvioRecord>` (multi-item) and `FluvioRecord`
/// (single-item) streams, so burst wrapping is never required in user code.
pub trait FluvioPubOperators {
    /// Write this stream to a Fluvio topic.
    ///
    /// - `conn` — Fluvio cluster connection config.
    /// - `topic` — The Fluvio topic to produce to. Must already exist.
    #[must_use]
    fn fluvio_pub(
        self: &Rc<Self>,
        conn: FluvioConnection,
        topic: impl Into<String>,
    ) -> Rc<dyn Node>;
}

impl FluvioPubOperators for dyn Stream<Burst<FluvioRecord>> {
    fn fluvio_pub(
        self: &Rc<Self>,
        conn: FluvioConnection,
        topic: impl Into<String>,
    ) -> Rc<dyn Node> {
        fluvio_pub(conn, topic, self)
    }
}

impl FluvioPubOperators for dyn Stream<FluvioRecord> {
    fn fluvio_pub(
        self: &Rc<Self>,
        conn: FluvioConnection,
        topic: impl Into<String>,
    ) -> Rc<dyn Node> {
        let burst_stream = self.map(|record| burst![record]);
        fluvio_pub(conn, topic, &burst_stream)
    }
}
