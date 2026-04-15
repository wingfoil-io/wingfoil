//! Fluvio consumer producer — streams records from a Fluvio topic partition.

use super::{FluvioConnection, FluvioEvent};
use crate::nodes::{RunParams, produce_async};
use crate::types::*;
use fluvio::consumer::ConsumerConfigExt;
use fluvio::{Fluvio, FluvioClusterConfig, Offset};
use futures::StreamExt;
use std::rc::Rc;

/// Stream records from a Fluvio topic partition.
///
/// Connects to the Fluvio SC at `connection.endpoint` and continuously delivers
/// records from `topic` / `partition` as [`FluvioEvent`]s wrapped in a
/// [`Burst`]. Each burst contains exactly one event (records arrive one at a time
/// from the consumer stream).
///
/// # Arguments
///
/// - `connection` — Fluvio cluster configuration (SC endpoint).
/// - `topic` — The Fluvio topic to consume from.
/// - `partition` — Partition index (0-based). Use `0` for single-partition topics.
/// - `start_offset` — Where to begin consuming:
///   - `None` — from the beginning of the partition (`Offset::beginning()`).
///   - `Some(n)` — from absolute offset `n` (inclusive). Negative values are invalid.
///
/// # Errors
///
/// The graph stops and propagates an error if:
/// - The connection to the Fluvio SC fails.
/// - The consumer cannot be created (e.g. topic does not exist).
/// - An invalid `start_offset` is provided (e.g. negative).
/// - A record-level error is returned by the Fluvio cluster.
#[must_use]
pub fn fluvio_sub(
    connection: FluvioConnection,
    topic: impl Into<String>,
    partition: u32,
    start_offset: Option<i64>,
) -> Rc<dyn Stream<Burst<FluvioEvent>>> {
    let topic = topic.into();
    // Validate offset early: if negative, the stream will fail immediately on run.
    let validation_error = if let Some(n) = start_offset {
        if n < 0 {
            Some(anyhow::anyhow!(
                "start_offset must be non-negative, got {}",
                n
            ))
        } else {
            None
        }
    } else {
        None
    };

    produce_async(move |_ctx: RunParams| async move {
        // Check validation result first.
        if let Some(err) = validation_error {
            return Err(err);
        }

        let cluster_config = FluvioClusterConfig::new(&connection.endpoint);
        let client = Fluvio::connect_with_config(&cluster_config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio connect failed: {e}"))?;

        let offset = match start_offset {
            None => Offset::beginning(),
            Some(n) => Offset::absolute(n)
                .map_err(|e| anyhow::anyhow!("invalid fluvio offset {n}: {e}"))?,
        };

        let consumer_config = ConsumerConfigExt::builder()
            .topic(topic)
            .partition(partition)
            .offset_start(offset)
            .build()
            .map_err(|e| anyhow::anyhow!("fluvio consumer config failed: {e}"))?;

        let mut stream = client
            .consumer_with_config(consumer_config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio consumer create failed: {e}"))?;

        Ok(async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(record) => {
                        let event = FluvioEvent {
                            key: record.key().map(|k| k.to_vec()),
                            value: record.value().to_vec(),
                            offset: record.offset,
                        };
                        yield Ok((NanoTime::now(), event));
                    }
                    Err(e) => {
                        yield Err(anyhow::anyhow!("fluvio record error: {e:?}"));
                        break;
                    }
                }
            }
        })
    })
}
