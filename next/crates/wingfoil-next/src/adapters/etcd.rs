//! etcd adapter — a streaming key-prefix snapshot + live watch **source**
//! (`etcd_sub`) and a key-value PUT **sink** (`EtcdSinkOps::etcd_pub`) for the
//! etcd key-value store. It ports the classic `wingfoil::adapters::etcd` module
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
//!   `use wingfoil_next::adapters::etcd::EtcdSinkOps;`.
//!
//! # Deviations from classic
//!
//! Every classic *capability* (snapshot→watch, deletes, leases with keepalive
//! and revoke-on-shutdown, the `force` conditional write) is preserved. The
//! surface differs in three deliberate ways:
//!
//! 1. **The tokio runtime is the caller's.** Classic `etcd_sub`/`etcd_pub` hide
//!    a global runtime inside `produce_async`/`consume_async`. Next has no
//!    `consume_async`, and its
//!    [`produce_async`](crate::async_source::produce_async) takes the runtime
//!    [`Handle`](tokio::runtime::Handle) and [`RunParams`] explicitly (the
//!    producer task is spawned at wiring time, before the run). Both functions
//!    here follow that convention: the caller owns a `tokio::runtime::Runtime`
//!    and hands in its `handle()`.
//! 2. **The sink connects eagerly, at wiring time.** `etcd_pub` returns
//!    `Result` and a connection (or `lease_grant`) failure surfaces *before* the
//!    run, whereas classic connected lazily inside the async consumer so the
//!    error surfaced *during* the run. (Skill step 8 prefers wiring-time I/O
//!    errors; a lazily-connecting client still surfaces the failure on the first
//!    PUT.)
//! 3. **The sink is a trait only.** Classic exposed both a free `etcd_pub`
//!    function and an `EtcdPubOperators` trait; next folds the single public
//!    entry point into the [`EtcdSinkOps`] trait (renamed for the sink-as-trait
//!    convention shared with [`lines`](crate::adapters::lines) /
//!    [`csv`](crate::adapters::csv)).
//!
//! ## Runtime requirement (a `block_on` footgun)
//!
//! Because the sink drives its writes with [`Handle::block_on`] on the graph
//! thread (and the [`LeaseGuard`] revokes on `Drop` the same way), **the graph
//! must be built, run, and dropped from a non-async thread** — the ordinary
//! case (`main`, a `#[test]` fn). Driving it from *inside* an async context
//! (e.g. `rt.block_on(async { g.build().run(..) })`) makes those `block_on`
//! calls panic, and a panic in the `Drop` revoke during unwinding would abort.
//! The producer and keepalive tasks, by contrast, run on the runtime's own
//! workers via [`Handle::spawn`] and have no such constraint.
//!
//! # Subscribing to a key prefix (the snapshot→watch handoff)
//!
//! [`etcd_sub`] first emits a snapshot of all keys matching the prefix as
//! [`EtcdEventKind::Put`] events, then streams live watch events (puts and
//! deletes). The watch is opened **before** the GET so no write is missed in
//! the handoff window; any event already covered by the snapshot
//! (`mod_revision <= snapshot_rev`) is filtered out as a duplicate. Snapshot and
//! live events are stamped with `NanoTime::now()`, so — like classic — the
//! source is designed for `RunMode::RealTime`; a `HistoricalFrom` run replays
//! them at wall-clock instants rather than a historical timeline.
//!
//! # Sink
//!
//! [`EtcdSinkOps::etcd_pub`] connects at wiring time (a connection error surfaces
//! before the run) and issues one PUT per [`EtcdEntry`] in each burst, blocking
//! the graph thread on the runtime for the write so any failure aborts the run
//! deterministically with context — matching the classic consumer's ordering
//! guarantees.
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

use std::cell::RefCell;
use std::time::Duration;

use anyhow::{Context, Result};
use etcd_client::{Client, Compare, CompareOp, GetOptions, PutOptions, Txn, TxnOp, WatchOptions};
use futures::StreamExt;
use tokio::runtime::Handle;
use wingfoil::NanoTime;

use crate::Burst;
use crate::async_source::{RunParams, produce_async};
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
/// `handle` is the caller's tokio runtime handle and `params` describes the run
/// the graph will be driven with (see [`produce_async`] and the module docs).
/// Use `.collapse()` for single-event processing.
#[must_use]
pub fn etcd_sub(
    g: &GraphBuilder,
    handle: &Handle,
    params: RunParams,
    conn: impl Into<EtcdConnection>,
    prefix: impl Into<String>,
) -> Stream<Burst<EtcdEvent>> {
    let conn = conn.into();
    let prefix = prefix.into();
    produce_async(g, handle, params, move |_p: RunParams| async move {
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

/// Holds a granted lease alive and revokes it on drop.
///
/// Moved into the sink's `for_each` closure, so it is dropped when the sink (and
/// with it the whole graph) is torn down: the keepalive task is aborted and the
/// lease revoked, matching classic's "leased keys vanish on clean shutdown".
struct LeaseGuard {
    handle: Handle,
    client: Client,
    lease_id: i64,
    keepalive: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let keepalive = self.keepalive.take();
        let mut client = self.client.clone();
        let lease_id = self.lease_id;
        // Best-effort revoke on teardown; a failure here (e.g. the connection is
        // already gone) is not worth aborting a shutting-down graph over.
        let _ = self.handle.block_on(async move {
            // Stop keepalive fully before revoking (as classic does), so a
            // renewal can't race the revoke. Awaiting an aborted handle resolves
            // immediately with a Cancelled error, which we ignore.
            if let Some(ka) = keepalive {
                ka.abort();
                let _ = ka.await;
            }
            client.lease_revoke(lease_id).await
        });
    }
}

/// Extension trait providing a fluent API for writing streams to etcd via PUT.
///
/// Implemented for both `Stream<Burst<EtcdEntry>>` (multi-item) and
/// `Stream<EtcdEntry>` (single-item, auto-wrapped into one-element bursts), so
/// burst wrapping is never required in user code.
pub trait EtcdSinkOps {
    /// Write this stream to etcd via PUT. Returns the sink `Stream<()>`.
    ///
    /// - `handle`: the caller's tokio runtime handle (see the module docs).
    /// - `lease_ttl`: `None` for plain writes; `Some(duration)` to attach a lease
    ///   with automatic keepalive renewal (keys vanish on sink teardown via
    ///   revoke).
    /// - `force`: `true` silently overwrites existing keys; `false` aborts the
    ///   run, naming the key, if it already exists (a conditional transaction).
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time if the etcd connection (or, when a lease
    /// is requested, the `lease_grant`) fails. A per-write failure (including a
    /// `force: false` conflict) aborts the run with context.
    fn etcd_pub(
        &self,
        handle: &Handle,
        conn: impl Into<EtcdConnection>,
        lease_ttl: Option<Duration>,
        force: bool,
    ) -> Result<Stream<()>>;
}

impl EtcdSinkOps for Stream<Burst<EtcdEntry>> {
    fn etcd_pub(
        &self,
        handle: &Handle,
        conn: impl Into<EtcdConnection>,
        lease_ttl: Option<Duration>,
        force: bool,
    ) -> Result<Stream<()>> {
        let conn = conn.into();
        // Connect at wiring time so a connection error surfaces before the run.
        let mut client = handle
            .block_on(Client::connect(&conn.endpoints, None))
            .with_context(|| format!("etcd_pub: connecting to etcd at {:?}", conn.endpoints))?;

        // Optionally grant a lease and start a background keepalive task.
        let guard = match lease_ttl {
            None => None,
            Some(ttl) => {
                // etcd's minimum TTL is 1 second; sub-second durations round up.
                let ttl_secs = ttl.as_secs().max(1) as i64;
                let lease_id = handle
                    .block_on(client.lease_grant(ttl_secs, None))
                    .context("etcd_pub: lease_grant failed")?
                    .id();

                // Keepalive runs on a clone so the sink's client stays free for
                // PUTs; it renews at ttl/3 (>= 1s) until the task is aborted.
                let mut ka_client = client.clone();
                let renew_interval = (ttl / 3).max(Duration::from_secs(1));
                let keepalive = handle.spawn(async move {
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

                Some(LeaseGuard {
                    handle: handle.clone(),
                    client: client.clone(),
                    lease_id,
                    keepalive: Some(keepalive),
                })
            }
        };

        let lease_id = guard.as_ref().map(|g| g.lease_id);
        let handle = handle.clone();
        // `for_each` takes an `Fn`; the etcd `Client` needs `&mut` per PUT, so it
        // lives behind a `RefCell`. The lease guard is moved in so its revoke
        // fires when the sink is dropped at graph teardown.
        let client = RefCell::new(client);
        Ok(self.for_each(move |burst: &Burst<EtcdEntry>| {
            // Load-bearing: referencing `guard` is what makes this `move` closure
            // capture (and thus own) it, so the lease is revoked only when the
            // sink is dropped at teardown — not immediately at the end of wiring.
            let _lease = &guard;
            let mut client = client.borrow_mut();
            handle.block_on(async {
                for entry in burst.iter() {
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
                        let put_op =
                            vec![TxnOp::put(entry.key.as_bytes(), entry.value.as_slice(), opts)];
                        let txn = Txn::new().when(key_absent).and_then(put_op);
                        let resp = client
                            .txn(txn)
                            .await
                            .with_context(|| format!("etcd_pub: conditional PUT {} failed", entry.key))?;
                        if !resp.succeeded() {
                            anyhow::bail!(
                                "etcd_pub: conditional write failed: key already exists (use force=true to overwrite): {}",
                                entry.key
                            );
                        }
                    }
                }
                anyhow::Ok(())
            })?;
            Ok(())
        }))
    }
}

impl EtcdSinkOps for Stream<EtcdEntry> {
    fn etcd_pub(
        &self,
        handle: &Handle,
        conn: impl Into<EtcdConnection>,
        lease_ttl: Option<Duration>,
        force: bool,
    ) -> Result<Stream<()>> {
        self.map(|entry: &EtcdEntry| burst![entry.clone()])
            .etcd_pub(handle, conn, lease_ttl, force)
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
