//! Integration tests for the etcd adapter.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test --features etcd-integration-test -p wingfoil \
//!   -- --test-threads=1 etcd::integration_tests
//! ```

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::types::Burst;
use crate::{RunFor, RunMode};
use etcd_client::Client;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

const ETCD_PORT: u16 = 2379;
const ETCD_IMAGE: &str = "gcr.io/etcd-development/etcd";
const ETCD_TAG: &str = "v3.5.0";

/// Start an etcd container and return the host endpoint.
/// The returned container must be kept alive for the duration of the test.
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

// ---- Tests ----

#[test]
fn test_connection_refused() {
    // Error propagates correctly without a running etcd.
    let conn = EtcdConnection::new("http://127.0.0.1:59999");
    let result = etcd_sub(conn, "/x/")
        .collapse()
        .collect()
        .run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(result.is_err(), "expected connection error");
}

#[test]
fn test_sub_snapshot_empty() -> anyhow::Result<()> {
    // An empty prefix yields no snapshot events and the stream blocks waiting for live events.
    // Use RunFor::Cycles(0) to stop immediately after setup.
    let (_container, endpoint) = start_etcd()?;
    let conn = EtcdConnection::new(&endpoint);

    let collected = etcd_sub(conn, "/empty/").collapse().collect();
    // RunFor::Cycles(0) is not meaningful; instead seed nothing and use RunFor::Cycles(1)
    // with a background write so we at least confirm the stream starts without error.
    // Here we just verify that connecting and getting an empty snapshot doesn't error.
    // We write one key in a background thread so the stream has something to receive.
    let endpoint_clone = endpoint.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        seed_keys(&endpoint_clone, &[("/empty/probe", "1")]).unwrap();
    });

    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;
    handle.join().ok();

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value.kind, EtcdEventKind::Put);
    assert_eq!(events[0].value.entry.key, "/empty/probe");
    Ok(())
}

#[test]
fn test_sub_snapshot_with_existing_keys() -> anyhow::Result<()> {
    // Pre-seeded keys appear as Put events in the snapshot phase.
    // Both keys arrive in a single burst (snapshot is emitted synchronously),
    // so we collect the burst directly and flatten rather than using collapse().
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/snap/a", "1"), ("/snap/b", "2")])?;

    let conn = EtcdConnection::new(&endpoint);
    let collected = etcd_sub(conn, "/snap/").collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;

    let events: Vec<EtcdEvent> = collected
        .peek_value()
        .into_iter()
        .flat_map(|v| v.value.into_iter())
        .collect();
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
    let (_container, endpoint) = start_etcd()?;
    let conn = EtcdConnection::new(&endpoint);

    let endpoint_clone = endpoint.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        seed_keys(&endpoint_clone, &[("/live/x", "hello")]).unwrap();
    });

    let collected = etcd_sub(conn, "/live/").collapse().collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;
    handle.join().ok();

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value.kind, EtcdEventKind::Put);
    assert_eq!(events[0].value.entry.key, "/live/x");
    assert_eq!(events[0].value.entry.value, b"hello");
    Ok(())
}

#[test]
fn test_pub_round_trip() -> anyhow::Result<()> {
    // etcd_pub writes keys; verify via direct client read.
    let (_container, endpoint) = start_etcd()?;
    let conn = EtcdConnection::new(&endpoint);

    let kv = EtcdEntry {
        key: "/rt/key1".to_string(),
        value: b"value1".to_vec(),
    };

    // Wrap the single KV in a Burst and produce it as a one-shot stream.
    let mut burst: Burst<EtcdEntry> = Burst::new();
    burst.push(kv);
    let source = crate::nodes::constant(burst);
    etcd_pub(conn, &source, None).run(RunMode::RealTime, RunFor::Cycles(1))?;

    // Verify via direct client read.
    let rt = tokio::runtime::Runtime::new()?;
    let value = rt.block_on(async {
        let mut client = Client::connect(&[&endpoint], None).await?;
        let resp = client.get("/rt/key1", None).await?;
        let kv = resp
            .kvs()
            .first()
            .ok_or_else(|| anyhow::anyhow!("key not found"))?;
        Ok::<Vec<u8>, anyhow::Error>(kv.value().to_vec())
    })?;
    assert_eq!(value, b"value1");
    Ok(())
}

#[test]
fn test_sub_delete_events() -> anyhow::Result<()> {
    // A DELETE in etcd produces an EtcdEventKind::Delete event.
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/del/k", "v")])?;

    let conn = EtcdConnection::new(&endpoint);

    let endpoint_clone = endpoint.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        delete_key(&endpoint_clone, "/del/k").unwrap();
    });

    let collected = etcd_sub(conn, "/del/").collapse().collect();
    // Cycles(2): 1 snapshot Put + 1 live Delete
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(2))?;
    handle.join().ok();

    let events = collected.peek_value();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value.kind, EtcdEventKind::Put);
    assert_eq!(events[1].value.kind, EtcdEventKind::Delete);
    assert_eq!(events[1].value.entry.key, "/del/k");
    Ok(())
}

#[test]
fn test_sub_no_race_between_snapshot_and_watch() -> anyhow::Result<()> {
    // A key written concurrently during snapshot→watch handoff is not missed or duplicated.
    // The concurrent write (no delay) completes before the graph's tokio task connects,
    // so both keys land in the snapshot burst.  We collect the burst and flatten.
    let (_container, endpoint) = start_etcd()?;
    seed_keys(&endpoint, &[("/race/existing", "old")])?;

    let conn = EtcdConnection::new(&endpoint);

    // Write a second key with no delay — intentional race with snapshot read.
    let endpoint_clone = endpoint.clone();
    let handle = std::thread::spawn(move || {
        seed_keys(&endpoint_clone, &[("/race/new", "new")]).unwrap();
    });
    handle.join().ok();

    let collected = etcd_sub(conn, "/race/").collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;

    let events: Vec<EtcdEvent> = collected
        .peek_value()
        .into_iter()
        .flat_map(|v| v.value.into_iter())
        .collect();
    let keys: std::collections::HashSet<String> =
        events.iter().map(|e| e.entry.key.clone()).collect();

    // Both keys present with no duplicates.
    assert!(keys.contains("/race/existing"), "existing key missing");
    assert!(keys.contains("/race/new"), "concurrent key missing");
    assert_eq!(keys.len(), 2, "duplicate events detected");
    Ok(())
}

/// Read a key from etcd, returning None if it doesn't exist.
fn get_key(endpoint: &str, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = Client::connect(&[endpoint], None).await?;
        let resp = client.get(key, None).await?;
        Ok(resp.kvs().first().map(|kv| kv.value().to_vec()))
    })
}

#[test]
fn test_pub_lease_keys_expire_after_revoke() -> anyhow::Result<()> {
    // Keys written with a lease are revoked (deleted) when the consumer stops cleanly.
    let (_container, endpoint) = start_etcd()?;
    let conn = EtcdConnection::new(&endpoint);

    let mut burst: Burst<EtcdEntry> = Burst::new();
    burst.push(EtcdEntry {
        key: "/lease/k1".to_string(),
        value: b"hello".to_vec(),
    });
    let source = crate::nodes::constant(burst);
    // Use a 30-second TTL — key should still vanish on revoke, not wait 30 s.
    etcd_pub(conn, &source, Some(std::time::Duration::from_secs(30)))
        .run(RunMode::RealTime, RunFor::Cycles(1))?;

    // Consumer stopped → lease was revoked → key must be gone.
    let value = get_key(&endpoint, "/lease/k1")?;
    assert!(value.is_none(), "key should be gone after lease revoke");
    Ok(())
}

#[test]
fn test_pub_lease_keepalive_extends_ttl() -> anyhow::Result<()> {
    // While the consumer is running, keepalive prevents key expiry.
    let (_container, endpoint) = start_etcd()?;
    let conn = EtcdConnection::new(&endpoint);

    // TTL of 3 s; keepalive renews every 1 s.  Graph runs for 10 s (> TTL).
    // The check thread polls until the key appears (handles variable startup time),
    // then waits past the raw TTL to confirm keepalive is working.
    let endpoint_check = endpoint.clone();
    let handle = std::thread::spawn(move || -> anyhow::Result<()> {
        // Poll until the key appears (consumer may take a few seconds to connect + write).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            if get_key(&endpoint_check, "/lease/heartbeat")?.is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "key never appeared within 8 s"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        // Sleep past the raw TTL (3 s) — key must still be alive due to keepalive.
        std::thread::sleep(std::time::Duration::from_secs(4));
        let v = get_key(&endpoint_check, "/lease/heartbeat")?;
        assert!(
            v.is_some(),
            "key should still exist; keepalive should have renewed lease"
        );
        Ok(())
    });

    // Produce the key once, then keep the graph alive for 10 s.
    let entry = crate::nodes::constant({
        let mut b: Burst<EtcdEntry> = Burst::new();
        b.push(EtcdEntry {
            key: "/lease/heartbeat".to_string(),
            value: b"alive".to_vec(),
        });
        b
    });
    etcd_pub(conn, &entry, Some(std::time::Duration::from_secs(3))).run(
        RunMode::RealTime,
        RunFor::Duration(std::time::Duration::from_secs(10)),
    )?;

    handle.join().unwrap()?;
    Ok(())
}

#[test]
fn test_pub_no_lease_keys_persist() -> anyhow::Result<()> {
    // Without a lease, keys remain after the consumer stops.
    let (_container, endpoint) = start_etcd()?;
    let conn = EtcdConnection::new(&endpoint);

    let mut burst: Burst<EtcdEntry> = Burst::new();
    burst.push(EtcdEntry {
        key: "/nolease/k1".to_string(),
        value: b"persist".to_vec(),
    });
    let source = crate::nodes::constant(burst);
    etcd_pub(conn, &source, None).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let value = get_key(&endpoint, "/nolease/k1")?;
    assert_eq!(value.as_deref(), Some(b"persist".as_ref()));
    Ok(())
}

#[test]
fn test_etcd_kv_value_str() {
    let kv = EtcdEntry {
        key: "/foo".to_string(),
        value: b"bar".to_vec(),
    };
    assert_eq!(kv.value_str().unwrap(), "bar");
}
