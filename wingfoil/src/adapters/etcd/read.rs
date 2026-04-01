//! etcd watch producer — streams a consistent snapshot followed by live watch events.

use super::{EtcdConnection, EtcdEntry, EtcdEvent, EtcdEventKind};
use crate::nodes::{RunParams, produce_async};
use crate::types::*;
use etcd_client::{Client, GetOptions, WatchOptions};
use futures::StreamExt;
use std::rc::Rc;

/// Stream all current KVs under `prefix` as [`EtcdEvent`]s, then stream live updates.
///
/// Always starts with a consistent snapshot (via GET), then seamlessly transitions
/// to a live watch. The watch is opened **before** the GET so no writes are missed
/// in the handoff window.
///
/// Emits `Burst<EtcdEvent>`. Use `.collapse()` for single-event processing.
///
/// # Race prevention
///
/// ```text
/// WATCH registered ──→ GET (snapshot_rev) ──→ emit snapshot ──→ watch events
///                                                               (skip mod_rev ≤ snapshot_rev)
/// ```
///
/// Any write committed between watch registration and the GET will appear in the watch
/// stream with `mod_revision > snapshot_rev` and will not be skipped. Any write already
/// visible in the GET has `mod_revision ≤ snapshot_rev` and is filtered out as a duplicate.
#[must_use]
pub fn etcd_sub(
    connection: EtcdConnection,
    prefix: impl Into<String>,
) -> Rc<dyn Stream<Burst<EtcdEvent>>> {
    let prefix = prefix.into();
    produce_async(move |_ctx: RunParams| async move {
        let mut client = Client::connect(&connection.endpoints, None).await?;

        // 1. Open the watch BEFORE the GET to prevent the snapshot/watch race.
        let watch_opts = WatchOptions::new().with_prefix();
        let mut watch_stream = client
            .watch(prefix.as_bytes(), Some(watch_opts))
            .await
            .map_err(|e| anyhow::anyhow!("etcd watch failed: {e}"))?;

        // 2. Read the snapshot and capture its revision.
        let get_opts = GetOptions::new().with_prefix();
        let get_resp = client
            .get(prefix.as_bytes(), Some(get_opts))
            .await
            .map_err(|e| anyhow::anyhow!("etcd get failed: {e}"))?;
        let snapshot_rev = get_resp.header().map(|h| h.revision()).unwrap_or(0);

        // 3. Collect snapshot KVs into owned data to move into the stream.
        let snapshot: Vec<EtcdEvent> = get_resp
            .kvs()
            .iter()
            .map(|kv| EtcdEvent {
                kind: EtcdEventKind::Put,
                entry: EtcdEntry {
                    key: String::from_utf8_lossy(kv.key()).into_owned(),
                    value: kv.value().to_vec(),
                },
                revision: snapshot_rev,
            })
            .collect();

        // 4. Return the combined snapshot + live stream.
        Ok(async_stream::stream! {
            // Phase 1: emit snapshot events.
            for event in snapshot {
                yield Ok((NanoTime::now(), event));
            }

            // Phase 2: drain the watch stream, deduplicating against the snapshot.
            loop {
                match watch_stream.next().await {
                    Some(Ok(resp)) => {
                        // Skip the initial "watch created" confirmation (no events).
                        if resp.created() {
                            continue;
                        }
                        if resp.canceled() {
                            yield Err(anyhow::anyhow!(
                                "etcd watch cancelled: {}",
                                resp.cancel_reason()
                            ));
                            break;
                        }
                        for event in resp.events() {
                            let kv = match event.kv() {
                                Some(kv) => kv,
                                None => continue,
                            };
                            let mod_rev = kv.mod_revision();
                            // Skip events already covered by the snapshot.
                            if mod_rev <= snapshot_rev {
                                continue;
                            }
                            let kind = match event.event_type() {
                                etcd_client::EventType::Put => EtcdEventKind::Put,
                                etcd_client::EventType::Delete => EtcdEventKind::Delete,
                            };
                            yield Ok((NanoTime::now(), EtcdEvent {
                                kind,
                                entry: EtcdEntry {
                                    key: String::from_utf8_lossy(kv.key()).into_owned(),
                                    value: kv.value().to_vec(),
                                },
                                revision: mod_rev,
                            }));
                        }
                    }
                    Some(Err(e)) => {
                        yield Err(anyhow::anyhow!("etcd watch error: {e}"));
                        break;
                    }
                    None => {
                        yield Err(anyhow::anyhow!("etcd watch stream closed unexpectedly"));
                        break;
                    }
                }
            }
        })
    })
}
