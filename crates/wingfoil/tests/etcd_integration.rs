//! Integration tests for the etcd adapter — parity port of the legacy
//! `legacy/wingfoil/src/adapters/etcd/integration_tests.rs`.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features etcd-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "etcd-integration-test")]

use std::time::Duration;

use etcd_client::Client;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
use wingfoil::adapters::etcd::{
    EtcdConnection, EtcdEntry, EtcdEvent, EtcdEventKind, EtcdSinkOps, etcd_sub,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const ETCD_PORT: u16 = 2379;
const ETCD_IMAGE: &str = "gcr.io/etcd-development/etcd";
const ETCD_TAG: &str = "v3.5.0";

/// Start an etcd container and return the host endpoint. The returned container
/// must be kept alive for the duration of the test.
fn start_etcd() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new(ETCD_IMAGE, ETCD_TAG)
        .with_wait_for(WaitFor::message_on_stderr(
            "now serving peer/client/metrics",
        ))
        .with_env_var("ETCD_LISTEN_CLIENT_URLS", "http://0.0.0.0:2379")
        .with_env_var("ETCD_ADVERTISE_CLIENT_URLS", "http://0.0.0.0:2379")
        .start()?;
    let port = container.get_host_port_ipv4(ETCD_PORT)?;
    let endpoint = format!("http://127.0.0.1:{port}");
    Ok((container, endpoint))
}

/// A realtime run description for the source (start time is ignored in realtime).
fn realtime(cycles: u32) -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Cycles(cycles),
        start_time: NanoTime::ZERO,
    }
}

/// The bound for a source test that must observe *every* event of a multi-value
/// snapshot or watch sequence: a fixed realtime window, never `RunFor::Cycles(n)`.
///
/// `etcd_sub` is realtime-only, and the realtime channel receiver groups a burst
/// by **arrival** — a cycle emits whatever `try_recv` drains at that moment —
/// not by the producer's timestamp (only the *historical* receiver groups by
/// timestamp; see `Builder::channel`). The producer sends one value per
/// `send_at`, and the first send already wakes the kernel, so a two-key snapshot
/// stamped at one instant can still be split: cycle 1 takes key A, and key B
/// rides cycle 2. Bounding by cycle count then ends the run holding only key A —
/// exactly the intermittent "concurrent key missing" failure this suite hit.
///
/// A window bound is immune: `collapse_accumulate` gathers across cycles, so it
/// does not matter how the events are distributed over them. Sized generously
/// against a slow CI container (the events themselves land in milliseconds); a
/// realtime run with nothing scheduled parks on the ready channel until the
/// bound, so this is the cost only when the graph is genuinely idle.
fn observe_window() -> RunFor {
    RunFor::Duration(Duration::from_secs(2))
}

/// Seed key-value pairs directly into etcd via the client.
fn seed_keys(endpoint: &str, pairs: &[(&str, &str)]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = Client::connect(&[endpoint], None).await?;
        for (k, v) in pairs {
            client.put(*k, *v, None).await?;
        }
        Ok(())
    })
}

/// Delete a key directly via the client.
fn delete_key(endpoint: &str, key: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = Client::connect(&[endpoint], None).await?;
        client.delete(key, None).await?;
        Ok(())
    })
}

/// Read a key from etcd, returning `None` if it doesn't exist.
fn get_key(endpoint: &str, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = Client::connect(&[endpoint], None).await?;
        let resp = client.get(key, None).await?;
        Ok(resp.kvs().first().map(|kv| kv.value().to_vec()))
    })
}

fn entry(key: &str, value: &[u8]) -> EtcdEntry {
    EtcdEntry {
        key: key.to_string(),
        value: value.to_vec(),
    }
}

// ---- Source tests ----

#[test]
fn test_sub_empty_snapshot_then_live_write() -> anyhow::Result<()> {
    // An empty prefix yields no snapshot events; a subsequent live write arrives
    // as a Put.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let events = etcd_sub(
        &g,
        realtime(1).run_mode,
        EtcdConnection::new(&endpoint),
        "/empty/",
    )?
    .collapse_accumulate();

    let endpoint_clone = endpoint.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        seed_keys(&endpoint_clone, &[("/empty/probe", "1")]).unwrap();
    });

    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Cycles(1))?;
    writer.join().unwrap();

    let events: Vec<EtcdEvent> = runner.value(&events);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EtcdEventKind::Put);
    assert_eq!(events[0].entry.key, "/empty/probe");
    Ok(())
}

#[test]
fn test_sub_snapshot_with_existing_keys() -> anyhow::Result<()> {
    // Pre-seeded keys appear as Put events in the snapshot phase. Bounded by a
    // window, not a cycle count — the realtime receiver may split the snapshot
    // across cycles (see `observe_window`).
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/snap/a", "1"), ("/snap/b", "2")])?;

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let events = etcd_sub(
        &g,
        realtime(1).run_mode,
        EtcdConnection::new(&endpoint),
        "/snap/",
    )?
    .collapse_accumulate();

    let mut runner = g.build();
    runner.run(RunMode::RealTime, observe_window())?;

    let events: Vec<EtcdEvent> = runner.value(&events);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.kind == EtcdEventKind::Put));

    let keys: std::collections::BTreeSet<String> =
        events.iter().map(|e| e.entry.key.clone()).collect();
    assert!(keys.contains("/snap/a"));
    assert!(keys.contains("/snap/b"));
    Ok(())
}

#[test]
fn test_sub_live_updates() -> anyhow::Result<()> {
    // Live watch events arrive after the (empty) snapshot phase.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    let endpoint_clone = endpoint.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        seed_keys(&endpoint_clone, &[("/live/x", "hello")]).unwrap();
    });

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let events = etcd_sub(
        &g,
        realtime(1).run_mode,
        EtcdConnection::new(&endpoint),
        "/live/",
    )?
    .collapse_accumulate();

    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Cycles(1))?;
    writer.join().unwrap();

    let events: Vec<EtcdEvent> = runner.value(&events);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EtcdEventKind::Put);
    assert_eq!(events[0].entry.key, "/live/x");
    assert_eq!(events[0].entry.value, b"hello");
    Ok(())
}

#[test]
fn test_sub_delete_events() -> anyhow::Result<()> {
    // A DELETE in etcd produces an EtcdEventKind::Delete event.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/del/k", "v")])?;

    let endpoint_clone = endpoint.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        delete_key(&endpoint_clone, "/del/k").unwrap();
    });

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    // 1 snapshot Put + 1 live Delete. A window, not `Cycles(2)`: if the delete
    // lands fast enough to ride the *same* burst as the snapshot Put, a
    // two-cycle bound would wait forever for a second cycle that never comes
    // (a realtime run with nothing scheduled parks until an external wake).
    let events = etcd_sub(
        &g,
        realtime(2).run_mode,
        EtcdConnection::new(&endpoint),
        "/del/",
    )?
    .collapse_accumulate();

    let mut runner = g.build();
    runner.run(RunMode::RealTime, observe_window())?;
    writer.join().unwrap();

    let events: Vec<EtcdEvent> = runner.value(&events);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, EtcdEventKind::Put);
    assert_eq!(events[1].kind, EtcdEventKind::Delete);
    assert_eq!(events[1].entry.key, "/del/k");
    Ok(())
}

#[test]
fn test_sub_no_race_between_snapshot_and_watch() -> anyhow::Result<()> {
    // A pre-seeded key and a second key written just before the graph starts
    // must each appear exactly once — no missed events, no duplicates. Both
    // land before the run, so both come from the snapshot; the assertion that
    // bites is `keys.len() == 2`, i.e. the watch does not replay what the
    // snapshot already carried.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/race/existing", "old")])?;

    let endpoint_clone = endpoint.clone();
    let writer = std::thread::spawn(move || {
        seed_keys(&endpoint_clone, &[("/race/new", "new")]).unwrap();
    });
    writer.join().unwrap();

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let events = etcd_sub(
        &g,
        realtime(1).run_mode,
        EtcdConnection::new(&endpoint),
        "/race/",
    )?
    .collapse_accumulate();

    let mut runner = g.build();
    runner.run(RunMode::RealTime, observe_window())?;

    let events: Vec<EtcdEvent> = runner.value(&events);
    let keys: std::collections::HashSet<String> =
        events.iter().map(|e| e.entry.key.clone()).collect();
    assert!(keys.contains("/race/existing"), "existing key missing");
    assert!(keys.contains("/race/new"), "concurrent key missing");
    assert_eq!(keys.len(), 2, "duplicate events detected");
    Ok(())
}

// ---- Sink tests ----

#[test]
fn test_pub_round_trip() -> anyhow::Result<()> {
    // etcd_pub writes keys; verify via a direct client read.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g.constant(burst![entry("/rt/key1", b"value1")]).etcd_pub(
            EtcdConnection::new(&endpoint),
            None,
            true,
        )?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

    assert_eq!(
        get_key(&endpoint, "/rt/key1")?.as_deref(),
        Some(&b"value1"[..])
    );
    Ok(())
}

#[test]
fn test_pub_no_lease_keys_persist() -> anyhow::Result<()> {
    // Without a lease, keys remain after the consumer stops.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g
            .constant(burst![entry("/nolease/k1", b"persist")])
            .etcd_pub(EtcdConnection::new(&endpoint), None, true)?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

    assert_eq!(
        get_key(&endpoint, "/nolease/k1")?.as_deref(),
        Some(&b"persist"[..])
    );
    Ok(())
}

#[test]
fn test_pub_lease_keys_expire_after_revoke() -> anyhow::Result<()> {
    // Keys written with a lease are revoked (deleted) when the sink is dropped
    // at graph teardown.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        // A 30s TTL — the key must still vanish on revoke, not wait 30s.
        let _sink = g.constant(burst![entry("/lease/k1", b"hello")]).etcd_pub(
            EtcdConnection::new(&endpoint),
            Some(Duration::from_secs(30)),
            true,
        )?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }
    // Sink dropped → lease revoked → key must be gone.
    assert!(
        get_key(&endpoint, "/lease/k1")?.is_none(),
        "key should be gone after lease revoke"
    );
    Ok(())
}

#[test]
fn test_pub_lease_keepalive_extends_ttl() -> anyhow::Result<()> {
    // While the sink is running, keepalive prevents key expiry.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    // TTL 3s, keepalive renews every 1s, graph runs 10s (> TTL). The check
    // thread polls until the key appears, then waits past the raw TTL.
    let endpoint_check = endpoint.clone();
    let checker = std::thread::spawn(move || -> anyhow::Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if get_key(&endpoint_check, "/lease/heartbeat")?.is_some() {
                break;
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "key never appeared within 15s"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        // Sleep past the raw TTL (3s) — the key must still be alive via keepalive.
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            get_key(&endpoint_check, "/lease/heartbeat")?.is_some(),
            "key should still exist; keepalive should have renewed the lease"
        );
        Ok(())
    });

    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        // Drive the write from a ticker rather than a bare `constant`: under
        // `RunFor::Duration` a `constant` source emits no wake, so the graph
        // would never run the cycle that fires the PUT. Re-writing the same key
        // under the lease every 500ms is idempotent — and it is keepalive, not
        // the re-write, that holds the lease open past its 3s TTL (a PUT does not
        // reset the lease timer; if keepalive stalled the lease would expire and
        // the next PUT with the dead lease id would abort the run).
        let _sink = g
            .ticker(Duration::from_millis(500))
            .map(|_: &()| burst![entry("/lease/heartbeat", b"alive")])
            .etcd_pub(
                EtcdConnection::new(&endpoint),
                Some(Duration::from_secs(3)),
                true,
            )?;
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(10)))?;
    }

    checker.join().unwrap()?;
    Ok(())
}

#[test]
fn test_pub_force_true_overwrites() -> anyhow::Result<()> {
    // force: true silently overwrites an existing key.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/force/k", "original")])?;

    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g.constant(burst![entry("/force/k", b"updated")]).etcd_pub(
            EtcdConnection::new(&endpoint),
            None,
            true,
        )?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

    assert_eq!(
        get_key(&endpoint, "/force/k")?.as_deref(),
        Some(&b"updated"[..])
    );
    Ok(())
}

#[test]
fn test_pub_force_false_fails_if_exists() -> anyhow::Result<()> {
    // force: false aborts the run, naming the key, when it already exists.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/noforce/k", "original")])?;

    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .constant(burst![entry("/noforce/k", b"should-not-overwrite")])
        .etcd_pub(EtcdConnection::new(&endpoint), None, false)?;
    let result = g.build().run(RunMode::RealTime, RunFor::Cycles(1));

    assert!(result.is_err(), "expected error when key already exists");
    let err = result.unwrap_err();
    assert!(
        err.chain().any(|e| e.to_string().contains("/noforce/k")),
        "error chain should name the key: {err:#}"
    );

    // Original value must be unchanged.
    assert_eq!(
        get_key(&endpoint, "/noforce/k")?.as_deref(),
        Some(&b"original"[..])
    );
    Ok(())
}

#[test]
fn test_pub_force_false_succeeds_if_absent() -> anyhow::Result<()> {
    // force: false writes successfully when the key does not exist.
    let rt = tokio::runtime::Runtime::new()?;
    let (_container, endpoint) = start_etcd()?;

    {
        let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
        let _sink = g
            .constant(burst![entry("/noforce/new", b"value")])
            .etcd_pub(EtcdConnection::new(&endpoint), None, false)?;
        g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;
    }

    assert_eq!(
        get_key(&endpoint, "/noforce/new")?.as_deref(),
        Some(&b"value"[..])
    );
    Ok(())
}
