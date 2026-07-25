//! etcd service-discovery parity tests for the zmq adapter — a port of the
//! `etcd_tests` module from the classic
//! `wingfoil/src/adapters/zmq/integration_tests.rs`.
//!
//! These exercise the pluggable [`EtcdRegistry`] discovery backend: a publisher
//! registers its bound address in etcd under a lease, and a subscriber resolves
//! it by name. Requires Docker (an etcd container via testcontainers) and
//! `libzmq`. Run with:
//! ```sh
//! cargo test -p wingfoil-next --features zmq-etcd-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "zmq-etcd-integration-test")]

use std::time::Duration;

use etcd_client::Client;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::etcd::EtcdConnection;
use wingfoil_next::adapters::zmq::{EtcdRegistry, ZeroMqPub, zmq_sub};
use wingfoil_next::prelude::*;

/// Start an etcd container and return `(container_handle, connection)`. The
/// container stops when the handle is dropped.
fn start_etcd() -> anyhow::Result<(impl Drop, EtcdConnection)> {
    let container = GenericImage::new("gcr.io/etcd-development/etcd", "v3.5.0")
        .with_wait_for(WaitFor::message_on_stderr(
            "now serving peer/client/metrics",
        ))
        .with_env_var("ETCD_LISTEN_CLIENT_URLS", "http://0.0.0.0:2379")
        .with_env_var("ETCD_ADVERTISE_CLIENT_URLS", "http://0.0.0.0:2379")
        .start()?;
    let port = container.get_host_port_ipv4(2379)?;
    let conn = EtcdConnection::new(format!("http://127.0.0.1:{port}"));
    Ok((container, conn))
}

/// Poll etcd until `key` is present or `timeout` elapses, returning its value.
fn wait_for_key(conn: &EtcdConnection, key: &str, timeout: Duration) -> anyhow::Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let val = rt.block_on(async {
            let mut client = Client::connect(&conn.endpoints, None).await?;
            let resp = client.get(key, None).await?;
            anyhow::Ok(
                resp.kvs()
                    .first()
                    .and_then(|kv| kv.value_str().ok())
                    .map(|s| s.to_string()),
            )
        })?;
        if let Some(v) = val {
            return Ok(v);
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("key {key:?} not found in etcd within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read_key(conn: &EtcdConnection, key: &str) -> anyhow::Result<Option<String>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut client = Client::connect(&conn.endpoints, None).await?;
        let resp = client.get(key, None).await?;
        Ok(resp
            .kvs()
            .first()
            .and_then(|kv| kv.value_str().ok())
            .map(|s| s.to_string()))
    })
}

#[test]
fn sub_etcd_no_etcd_returns_error() {
    // No container — a lookup against an unreachable etcd must error at wiring.
    let conn = EtcdConnection::new("http://127.0.0.1:59999");
    let g = GraphBuilder::new();
    let result = zmq_sub::<u64>(&g, RunMode::RealTime, ("anything", EtcdRegistry::new(conn)));
    assert!(result.is_err(), "expected error when etcd is unreachable");
}

#[test]
fn sub_etcd_name_not_found() {
    let (_container, conn) = start_etcd().unwrap();
    let g = GraphBuilder::new();
    let result = zmq_sub::<u64>(
        &g,
        RunMode::RealTime,
        ("nonexistent-key", EtcdRegistry::new(conn)),
    );
    assert!(result.is_err(), "expected error for absent key");
    let msg = format!("{:#}", result.err().unwrap());
    assert!(
        msg.contains("no publisher named"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn pub_etcd_registers_address() {
    let (_container, conn) = start_etcd().unwrap();
    let port = 5721u16;

    let conn_pub = conn.clone();
    let handle = std::thread::spawn(move || -> anyhow::Result<()> {
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(Duration::from_millis(50))
            .count()
            .zmq_pub(port, ("etcd-quotes", EtcdRegistry::new(conn_pub)));
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
        Ok(())
    });

    let val = wait_for_key(&conn, "etcd-quotes", Duration::from_secs(5)).unwrap();
    assert!(
        val.contains(&port.to_string()),
        "address should contain the port: {val}"
    );
    handle.join().expect("publisher panicked").unwrap();
}

#[test]
fn sub_etcd_end_to_end() {
    let (_container, conn) = start_etcd().unwrap();
    let port = 5722u16;

    let conn_pub = conn.clone();
    std::thread::spawn(move || -> anyhow::Result<()> {
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(Duration::from_millis(50))
            .count()
            .map(|n: &u64| format!("{n}").into_bytes())
            .zmq_pub(port, ("etcd-data", EtcdRegistry::new(conn_pub)));
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))?;
        Ok(())
    });

    wait_for_key(&conn, "etcd-data", Duration::from_secs(5)).unwrap();

    let g = GraphBuilder::new();
    let (data, _status) = zmq_sub::<Vec<u8>>(
        &g,
        RunMode::RealTime,
        ("etcd-data", EtcdRegistry::new(conn)),
    )
    .unwrap();
    let received = data.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(1500)),
        )
        .unwrap();
    assert!(
        !runner.value(&received).is_empty(),
        "no data received via etcd discovery"
    );
}

#[test]
fn pub_etcd_lease_revoked_on_stop() {
    let (_container, conn) = start_etcd().unwrap();
    let port = 5723u16;

    let conn_pub = conn.clone();
    let handle = std::thread::spawn(move || -> anyhow::Result<()> {
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(Duration::from_millis(50))
            .count()
            .zmq_pub(port, ("etcd-lease-key", EtcdRegistry::new(conn_pub)));
        g.build().run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(300)),
        )?;
        Ok(())
    });
    handle.join().expect("publisher panicked").unwrap();

    // Give etcd a moment to process the revoke.
    std::thread::sleep(Duration::from_millis(200));
    let val = read_key(&conn, "etcd-lease-key").unwrap();
    assert!(
        val.is_none(),
        "key should be gone after publisher stop, got: {val:?}"
    );
}

#[test]
fn pub_etcd_historical_mode_fails() {
    // The run-mode check runs before the registry is touched, so a historical
    // run errors with "real-time" — not an etcd connection error — even against
    // an unreachable etcd. Parity of classic `zmq_pub_etcd_historical_mode_fails`.
    let conn = EtcdConnection::new("http://127.0.0.1:59999");
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .zmq_pub(5724, ("test-hist", EtcdRegistry::new(conn)));
    let result = g
        .build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));
    let err = result.expect_err("expected historical mode to fail");
    assert!(
        format!("{err:#}").contains("real-time"),
        "expected the error to mention real-time, got: {err:#}"
    );
}
