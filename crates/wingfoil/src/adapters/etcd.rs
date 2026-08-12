//! etcd adapter — a streaming key-prefix snapshot + live watch **source**
//! (`etcd_sub`) and a key-value PUT **sink** (`EtcdSinkOps::etcd_pub`) for the
//! etcd key-value store. It ports the legacy `wingfoil::adapters::etcd` module
//! onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`etcd_sub`] on a
//!   [`GraphBuilder`]: a consistent key-prefix snapshot followed by live watch
//!   events, emitting `Stream<Burst<EtcdEvent>>`.
//! - **Sink** — the [`EtcdSinkOps`] extension trait on `Stream<Burst<EtcdEntry>>`
//!   (and, for convenience, `Stream<EtcdEntry>`), enabled with
//!   `use wingfoil::adapters::etcd::EtcdSinkOps;`.
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* (snapshot→watch, deletes, leases with keepalive
//! and revoke-on-shutdown, the `force` conditional write) is preserved. The
//! surface differs in three deliberate ways:
//!
//! 1. **The graph owns the tokio runtime.** Legacy `etcd_sub`/`etcd_pub` hide a
//!    never-dropped global runtime inside `produce_async`/`consume_async`. Wingfoil's
//!    `GraphBuilder` instead owns one runtime, created lazily on first async use
//!    and dropped at teardown, shared by every async adapter in the graph — so
//!    the common call needs no `&Handle` and there is no leaked global (see
//!    `docs/decisions/runtime-ownership.md`). `etcd_sub` takes a [`RunMode`] (only to reject
//!    a historical run at wiring); the producer task spawns in `start()`, deferred
//!    via `source_at_start`, so the etcd connect + watch happen at run start, not
//!    at wiring, and the producer's `RunParams` come from the actual run. To embed
//!    the
//!    graph in an existing runtime, install it as an override with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime).
//! 2. **The sink connects lazily, on the first write.** Like legacy, `etcd_pub`
//!    connects (and grants any lease) inside the async consumer on the first
//!    PUT, so wiring opens no socket and a connection or `lease_grant` failure
//!    surfaces *during* the run (via `consume_async`'s error channel), not at
//!    graph construction. If the stream is empty, nothing is connected or leased.
//! 3. **The sink is a trait only.** Legacy exposed both a free `etcd_pub`
//!    function and an `EtcdPubOperators` trait; wingfoil folds the single public
//!    entry point into the [`EtcdSinkOps`] trait (renamed for the sink-as-trait
//!    convention shared with [`lines`](crate::adapters::lines) /
//!    [`csv`](crate::adapters::csv)).
//!
//! ## Runtime requirement (a `block_on` footgun)
//!
//! The connect, `lease_grant`, keepalive, and per-write PUTs all run **on** the
//! runtime's own workers — the connect + PUTs on the shared
//! [`consume_async`](crate::async_source::consume_async) consumer task (see
//! below), the keepalive via `tokio::spawn`. The one graph-thread `block_on` is
//! the **lease revoke at teardown** (`consume_async` sinks all `block_on` the
//! graph thread at teardown), so **the graph must be built, run, and dropped
//! from a non-async thread** — the ordinary case (`main`, a `#[test]` fn).
//! Driving it from *inside* an async context (e.g.
//! `rt.block_on(async { g.build().run(..) })`) makes that `block_on` panic. This
//! is the same constraint every `consume_async` sink carries.
//!
//! ## Writes run off the graph thread (via `consume_async`)
//!
//! `etcd_pub` drives its PUTs through the shared async-sink primitive
//! [`consume_async`](crate::async_source::consume_async), so the network writes
//! run on a background consumer task rather than blocking a `cycle`. A single
//! consumer awaits each write to completion before the next, preserving burst
//! order. A `force: false` conditional-write conflict returns an error that
//! aborts the run — surfaced on a later cycle, or, for the **final** write, by
//! the sink's `flush` teardown (`consume_async` returns it, wired here as
//! [`finally`](crate::fluent::StreamOps::finally)). That teardown-time surfacing
//! is exactly how legacy aborts a single-cycle (`RunFor::Cycles(1)`) run whose
//! only write conflicts (`AsyncConsumerNode::teardown` joins the consumer and
//! propagates its error), so the `force: false` guarantee is preserved without a
//! per-write `block_on`.
//!
//! # Subscribing to a key prefix (the snapshot→watch handoff)
//!
//! [`etcd_sub`] first emits a snapshot of all keys matching the prefix as
//! [`EtcdEventKind::Put`] events, then streams live watch events (puts and
//! deletes). The watch is opened **before** the GET so no write is missed in
//! the handoff window; any event already covered by the snapshot
//! (`mod_revision <= snapshot_rev`) is filtered out as a duplicate. Snapshot and
//! live events are stamped with `NanoTime::now()`: the source is a **live,
//! unbounded, wall-clock stream** with no historical timeline to replay, so it
//! runs under [`RunMode::RealTime`] only. A `HistoricalFrom` run is **rejected
//! at wiring time** — [`etcd_sub`] returns an error rather than deadlocking (the
//! channel receiver's historical path block-collects the whole stream up front,
//! and this watch never closes).
//!
//! # Sink
//!
//! [`EtcdSinkOps::etcd_pub`] connects lazily on the first write (a connection
//! error surfaces during the run) and issues one PUT per [`EtcdEntry`] in each
//! burst on the off-thread [`consume_async`](crate::async_source::consume_async)
//! consumer, in order, so any failure aborts the run with context — matching the
//! legacy consumer's ordering and error-surfacing guarantees.
//!
//! - `lease_ttl: None` writes plain keys that persist until deleted.
//! - `lease_ttl: Some(ttl)` attaches an etcd lease with a background keepalive
//!   task that renews it every `ttl/3`; the lease is **revoked** when the sink
//!   is dropped at graph teardown, so leased keys vanish immediately rather than
//!   waiting out the TTL (presence/heartbeat pattern).
//! - `force: true` silently overwrites an existing key; `force: false` issues a
//!   conditional transaction (`create_revision == 0`) and aborts the run,
//!   naming the key, if it already exists.
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 2379:2379 \
//!   -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//!   -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//!   gcr.io/etcd-development/etcd:v3.5.0
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use etcd_client::{Client, Compare, CompareOp, GetOptions, PutOptions, Txn, TxnOp, WatchOptions};
use futures::StreamExt;
use wingfoil::{NanoTime, RunMode};

use crate::Burst;
use crate::async_source::{RunParams, consume_async, produce_async};
use crate::burst;
use crate::fluent::{GraphBuilder, Stream, StreamOps};

/// Connection configuration for etcd.
#[derive(Debug, Clone)]
pub struct EtcdConnection {
    /// etcd endpoints, e.g. `["http://localhost:2379"]`.
    pub endpoints: Vec<String>,
}

impl EtcdConnection {
    /// Create a connection config with a single endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoints: vec![endpoint.into()],
        }
    }

    /// Create a connection config with multiple endpoints (for an etcd cluster).
    pub fn with_endpoints(endpoints: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            endpoints: endpoints.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&str> for EtcdConnection {
    fn from(endpoint: &str) -> Self {
        Self::new(endpoint)
    }
}

impl From<String> for EtcdConnection {
    fn from(endpoint: String) -> Self {
        Self::new(endpoint)
    }
}

impl From<&String> for EtcdConnection {
    fn from(endpoint: &String) -> Self {
        Self::new(endpoint.clone())
    }
}

/// A single key-value pair from etcd.
#[derive(Debug, Clone, Default)]
pub struct EtcdEntry {
    pub key: String,
    pub value: Vec<u8>,
}

impl EtcdEntry {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> std::result::Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }
}

/// The type of change represented by an [`EtcdEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EtcdEventKind {
    /// A key was created or updated.
    #[default]
    Put,
    /// A key was deleted.
    ///
    /// The associated [`EtcdEvent::entry`] carries the deleted key but an
    /// **empty value** (`value == vec![]`). Use [`EtcdEventKind::Put`] events if
    /// you need the value.
    Delete,
}

/// An event from an etcd watch stream.
///
/// Snapshot events (from the initial GET) are always [`EtcdEventKind::Put`].
/// Subsequent watch events reflect the actual change type from etcd.
#[derive(Debug, Clone, Default)]
pub struct EtcdEvent {
    pub kind: EtcdEventKind,
    pub entry: EtcdEntry,
    /// The etcd cluster revision at which this event was observed.
    pub revision: i64,
}

/// Stream all current KVs under `prefix` as [`EtcdEvent`]s, then stream live
/// updates, as a `Burst<EtcdEvent>` source.
///
/// Always starts with a consistent snapshot (via GET), then seamlessly
/// transitions to a live watch. The watch is opened **before** the GET so no
/// writes are missed in the handoff window; any watch event with
/// `mod_revision <= snapshot_rev` is filtered out as a duplicate of the
/// snapshot.
///
/// `run_mode` is the mode the graph will be driven with — used only to reject a
/// historical run at wiring (see below); the producer's full [`RunParams`] are
/// derived from the actual run at start (see [`produce_async`]). The graph owns
/// the tokio runtime. Iterate the burst to process every event — `.collapse()`
/// keeps only the burst's **last** event and silently drops the rest, which on
/// a watch stream loses updates (see [`Collapse`](crate::ops::Collapse)).
///
/// **The snapshot is not guaranteed to arrive in one burst.** All of its events
/// share one timestamp (one consistent read), but this is a realtime-only
/// source and the realtime channel receiver groups a burst by *arrival*, not by
/// timestamp — so a multi-key snapshot may be split across cycles. Nothing is
/// lost; only the cycle boundaries vary. Bound such a run by
/// [`RunFor::Duration`](crate::RunFor::Duration) (or `Forever`) and accumulate —
/// never by `RunFor::Cycles(n)`, which can end the run mid-snapshot.
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: the etcd watch is a live, unbounded,
/// wall-clock-stamped stream with no historical timeline to replay, and the
/// historical channel path would block-collect it up front and deadlock at
/// `start`. Run `etcd_sub` under [`RunMode::RealTime`].
pub fn etcd_sub(
    g: &GraphBuilder,
    run_mode: RunMode,
    conn: impl Into<EtcdConnection>,
    prefix: impl Into<String>,
) -> Result<Stream<Burst<EtcdEvent>>> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "etcd_sub: RunMode::HistoricalFrom is unsupported — the etcd watch is a \
             live, unbounded, wall-clock-stamped stream with no historical timeline \
             to replay; run etcd_sub under RunMode::RealTime"
        );
    }
    let conn = conn.into();
    let prefix = prefix.into();
    produce_async(
        g,
        move |_p: RunParams| async move {
            let mut client = Client::connect(&conn.endpoints, None)
                .await
                .with_context(|| format!("etcd_sub: connecting to etcd at {:?}", conn.endpoints))?;

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

            // 4. Return the combined snapshot + live stream. `watch_stream` is moved
            //    in and kept alive for the stream's lifetime so the watch stays open.
            Ok(async_stream::stream! {
                // Phase 1: emit the snapshot. Every snapshot KV shares ONE
                // timestamp — the whole GET is one consistent read at
                // `snapshot_rev`, so stamping each event with its own
                // `NanoTime::now()` would invent instants the data does not
                // have, and scatter one atomic read across them. This matches
                // legacy's single `HistoricalValue` snapshot burst.
                //
                // It does NOT mean the consumer sees the snapshot as one burst.
                // `etcd_sub` is realtime-only, and the realtime channel receiver
                // groups by **arrival** — a cycle emits whatever is queued at
                // that moment — not by timestamp; only the historical receiver
                // groups on the stamp (`Builder::channel`). The producer sends
                // one value per `send_at` and the first send already wakes the
                // kernel, so a multi-key snapshot may still be split across
                // cycles. Nothing is lost (the rest ride the next cycle), but a
                // consumer must not bound the run by cycle count and expect the
                // whole snapshot: use `RunFor::Duration`/`Forever` and
                // accumulate.
                let snapshot_time = NanoTime::now();
                for event in snapshot {
                    yield Ok((snapshot_time, event));
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
        },
        None,
    )
}

/// Holds a granted lease alive and revokes it on drop.
///
/// Lazily-established sink state, shared between the `consume_async` consumer
/// (which connects — and grants any lease — on the **first write**) and the
/// teardown closure (which revokes the lease). A single consumer task
/// establishes and uses it, and teardown runs only after that task has drained,
/// so the `Mutex` is never contended and is never locked across an `.await`.
struct EtcdSinkState {
    client: Client,
    lease_id: Option<i64>,
    keepalive: Option<tokio::task::JoinHandle<()>>,
}

/// Extension trait providing a fluent API for writing streams to etcd via PUT.
///
/// Implemented for both `Stream<Burst<EtcdEntry>>` (multi-item) and
/// `Stream<EtcdEntry>` (single-item, auto-wrapped into one-element bursts), so
/// burst wrapping is never required in user code.
pub trait EtcdSinkOps {
    /// Write this stream to etcd via PUT. Returns the sink `Stream<()>`.
    ///
    /// - `lease_ttl`: `None` for plain writes; `Some(duration)` to attach a lease
    ///   with automatic keepalive renewal (keys vanish on sink teardown via
    ///   revoke).
    /// - `force`: `true` silently overwrites existing keys; `false` aborts the
    ///   run, naming the key, if it already exists (a conditional transaction).
    ///
    /// The graph owns the tokio runtime (see the module docs).
    ///
    /// # Errors
    ///
    /// The connection (and any lease) is established lazily on the first write,
    /// so an etcd connection or `lease_grant` failure aborts the *run* (not
    /// wiring) with context. A per-write failure (including a `force: false`
    /// conflict) likewise aborts the run with context.
    fn etcd_pub(
        &self,
        conn: impl Into<EtcdConnection>,
        lease_ttl: Option<Duration>,
        force: bool,
    ) -> Result<Stream<()>>;
}

impl EtcdSinkOps for Stream<Burst<EtcdEntry>> {
    fn etcd_pub(
        &self,
        conn: impl Into<EtcdConnection>,
        lease_ttl: Option<Duration>,
        force: bool,
    ) -> Result<Stream<()>> {
        let conn = conn.into();
        // The graph owns the runtime; the sink connects/keepalives/revokes on it.
        let handle = self.graph().async_runtime_handle()?;

        // The connection (and any lease + keepalive) is established lazily on the
        // first write, on the consumer task — not here — so wiring opens no socket
        // and a connect/lease failure aborts the run, not graph construction
        // (matching legacy's connect-lazily-in-consumer). The established state
        // is shared back to the teardown closure so it can revoke the lease after
        // the writes drain. If no write ever runs, nothing is connected or leased.
        let endpoints = Arc::new(conn.endpoints.clone());
        let state: Arc<Mutex<Option<EtcdSinkState>>> = Arc::new(Mutex::new(None));

        // Each `EtcdEntry` is written by the shared `consume_async` consumer task,
        // off the graph thread (single consumer, so PUTs preserve burst order). A
        // `force:false` conflict returns an error that aborts the run — on a later
        // cycle, or, for the final write, via the `flush` teardown wired below
        // (matching legacy's teardown-time surfacing).
        let consumer_state = Arc::clone(&state);
        let (sink, flush) = consume_async(&self.graph(), None, move |entry: EtcdEntry| {
            let state = Arc::clone(&consumer_state);
            let endpoints = Arc::clone(&endpoints);
            async move {
                // Establish (connect + optional lease + keepalive) on the first
                // write. Bind the check to a `let` so the `Mutex` guard drops
                // before the `.await`s below (never held across an await).
                let needs_connect = state
                    .lock()
                    .expect("etcd_pub sink mutex poisoned")
                    .is_none();
                if needs_connect {
                    let mut client = Client::connect(endpoints.as_slice(), None)
                        .await
                        .with_context(|| {
                            format!("etcd_pub: connecting to etcd at {endpoints:?}")
                        })?;
                    let (lease_id, keepalive) = match lease_ttl {
                        None => (None, None),
                        Some(ttl) => {
                            // etcd's minimum TTL is 1 second; sub-second rounds up.
                            let ttl_secs = ttl.as_secs().max(1) as i64;
                            let lease_id = client
                                .lease_grant(ttl_secs, None)
                                .await
                                .context("etcd_pub: lease_grant failed")?
                                .id();
                            // Keepalive runs on a clone so the sink's client stays
                            // free for PUTs; it renews at ttl/3 (>= 1s) until
                            // aborted. We are on the runtime, so `tokio::spawn`.
                            let mut ka_client = client.clone();
                            let renew_interval = (ttl / 3).max(Duration::from_secs(1));
                            let keepalive = tokio::spawn(async move {
                                let (mut keeper, mut ka_stream) =
                                    match ka_client.lease_keep_alive(lease_id).await {
                                        Ok(pair) => pair,
                                        Err(_) => return,
                                    };
                                loop {
                                    tokio::time::sleep(renew_interval).await;
                                    if keeper.keep_alive().await.is_err() {
                                        break;
                                    }
                                    // Drain the server ack to keep the stream healthy.
                                    match ka_stream.message().await {
                                        Ok(Some(_)) => {}
                                        _ => break,
                                    }
                                }
                            });
                            (Some(lease_id), Some(keepalive))
                        }
                    };
                    *state.lock().expect("etcd_pub sink mutex poisoned") = Some(EtcdSinkState {
                        client,
                        lease_id,
                        keepalive,
                    });
                }

                // Clone the client and read the lease id for this write (guard
                // dropped before the `.await`); the etcd `Client` is an `Arc`
                // inside, so the clone is cheap and lets the write own its `&mut`.
                let (mut client, lease_id) = {
                    let guard = state.lock().expect("etcd_pub sink mutex poisoned");
                    let s = guard
                        .as_ref()
                        .expect("invariant: sink state established above");
                    (s.client.clone(), s.lease_id)
                };
                let opts = lease_id.map(|id| PutOptions::new().with_lease(id));
                if force {
                    client
                        .put(entry.key.clone(), entry.value.clone(), opts)
                        .await
                        .with_context(|| format!("etcd_pub: PUT {} failed", entry.key))?;
                } else {
                    // Conditional put: succeed only if the key is absent.
                    // create_revision == 0 is etcd's canonical "key absent".
                    let key_absent = vec![Compare::create_revision(
                        entry.key.as_bytes(),
                        CompareOp::Equal,
                        0,
                    )];
                    let put_op = vec![TxnOp::put(
                        entry.key.as_bytes(),
                        entry.value.as_slice(),
                        opts,
                    )];
                    let txn = Txn::new().when(key_absent).and_then(put_op);
                    let resp = client.txn(txn).await.with_context(|| {
                        format!("etcd_pub: conditional PUT {} failed", entry.key)
                    })?;
                    if !resp.succeeded() {
                        anyhow::bail!(
                            "etcd_pub: conditional write failed: key already exists (use force=true to overwrite): {}",
                            entry.key
                        );
                    }
                }
                Ok(())
            }
        })?;

        // Teardown: drain every queued write (surfacing a final-cycle error),
        // *then* revoke any lease so the revoke fires only after the writes
        // complete — the legacy order (writes end → abort keepalive → revoke).
        // The revoke runs via `block_on` on the graph thread (an A5a footgun,
        // like every `consume_async` sink), regardless of a write error, and is
        // best-effort (a failure on an already-gone connection is not worth
        // aborting a shutting-down graph over). If no write ever ran, no
        // connection was made and there is nothing to revoke.
        Ok(self.for_each(sink).finally(move |u: &()| {
            let flush_result = flush(u);
            // Take the established state (guard dropped before `block_on`). A
            // `None` means no write ever ran — nothing was connected or leased.
            let established = state.lock().expect("etcd_pub sink mutex poisoned").take();
            if let Some(EtcdSinkState {
                mut client,
                lease_id: Some(lease_id),
                keepalive,
            }) = established
            {
                let _ = handle.block_on(async move {
                    // Stop keepalive fully before revoking (as legacy does), so a
                    // renewal can't race the revoke. Awaiting an aborted handle
                    // resolves immediately with Cancelled, ignored.
                    if let Some(ka) = keepalive {
                        ka.abort();
                        let _ = ka.await;
                    }
                    client.lease_revoke(lease_id).await
                });
            }
            flush_result
        }))
    }
}

impl EtcdSinkOps for Stream<EtcdEntry> {
    fn etcd_pub(
        &self,
        conn: impl Into<EtcdConnection>,
        lease_ttl: Option<Duration>,
        force: bool,
    ) -> Result<Stream<()>> {
        self.map(|entry: &EtcdEntry| burst![entry.clone()])
            .etcd_pub(conn, lease_ttl, force)
    }
}

#[cfg(test)]
mod tests {
    use super::{EtcdConnection, EtcdEntry};

    #[test]
    fn value_str_reads_utf8() {
        let entry = EtcdEntry {
            key: "/foo".to_string(),
            value: b"bar".to_vec(),
        };
        assert_eq!(entry.value_str().unwrap(), "bar");
    }

    #[test]
    fn connection_from_str_and_endpoints() {
        let one = EtcdConnection::from("http://localhost:2379");
        assert_eq!(one.endpoints, vec!["http://localhost:2379".to_string()]);

        let many = EtcdConnection::with_endpoints(["http://a:2379", "http://b:2379"]);
        assert_eq!(
            many.endpoints,
            vec!["http://a:2379".to_string(), "http://b:2379".to_string()]
        );
    }
}
