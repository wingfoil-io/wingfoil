//! Integration tests for the fluvio adapter — parity port of the legacy
//! `legacy/wingfoil/src/adapters/fluvio/integration_tests.rs`.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test -p wingfoil-next --features fluvio-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
//!
//! # Container startup
//!
//! Tests spin up `infinyon/fluvio:0.18.1` via testcontainers using host
//! networking. The startup sequence is:
//! 1. Start the SC and wait for it to log "Streaming Controller started successfully".
//! 2. Register the custom SPU spec via FluvioAdmin — the SC must know about the
//!    SPU before the SPU process connects, otherwise the SC closes the connection.
//! 3. Exec the SPU process into the running container.
//! 4. Sleep 3 s for the SPU to complete registration.
//!
//! Host networking is required so the SPU's public endpoint (`127.0.0.1:9010`),
//! which the SC hands to clients, is reachable from the test process on the host.
#![cfg(feature = "fluvio-integration-test")]

use std::sync::{Arc, Mutex, Weak};

use fluvio::consumer::ConsumerConfigExt;
use fluvio::metadata::customspu::CustomSpuSpec;
use fluvio::metadata::topic::TopicSpec;
use fluvio::{Fluvio, FluvioAdmin, FluvioClusterConfig, Offset};
use fluvio_controlplane_metadata::spu::{EncryptionEnum, Endpoint, IngressPort};
use futures::StreamExt;
use testcontainers::{
    GenericImage, ImageExt, core::ExecCommand, core::WaitFor, runners::SyncRunner,
};
use wingfoil_next::adapters::fluvio::{
    FluvioConnection, FluvioEvent, FluvioRecord, FluvioSinkOps, fluvio_sub,
};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

const FLUVIO_SC_PORT: u16 = 9003;
const FLUVIO_SC_PRIVATE_PORT: u16 = 9004;
const FLUVIO_SPU_PORT: u16 = 9010;
const FLUVIO_SPU_PRIVATE_PORT: u16 = 9011;
const FLUVIO_IMAGE: &str = "infinyon/fluvio";
const FLUVIO_TAG: &str = "0.18.1";

/// A running Fluvio cluster, shared by every test in this binary.
///
/// The cluster uses **host networking on fixed ports** (the SPU public endpoint
/// the SC hands to clients must be reachable from the host as `127.0.0.1:9010`),
/// so at most one can exist per machine. `container` is `None` when we attached
/// to a cluster that was already up rather than starting our own.
struct FluvioCluster {
    #[allow(dead_code)]
    container: Option<testcontainers::Container<GenericImage>>,
    endpoint: String,
}

/// Hand out the shared cluster, starting it on first use.
///
/// Every test holds an `Arc` for its duration, so the container is stopped by
/// `Drop` once the last test finishes — no leaked container, and no per-test
/// startup cost.
///
/// **Why shared rather than one cluster per test:** host networking pins the SC
/// to 9003 and the SPU to 9010, and the SPU is registered under a fixed id
/// (`spu-5001`). A second cluster cannot bind those ports, so a per-test
/// container would silently attach to the first cluster's SC and then fail
/// registering an SPU that is `already defined`. That is exactly what happened
/// when the main `cargo test -p wingfoil-next --all-features` job ran this suite
/// without `--test-threads=1`. Sharing makes the real constraint explicit and
/// makes the suite safe at any thread count.
fn fluvio_cluster() -> anyhow::Result<Arc<FluvioCluster>> {
    static CLUSTER: Mutex<Weak<FluvioCluster>> = Mutex::new(Weak::new());
    let mut slot = CLUSTER.lock().expect("fluvio cluster mutex poisoned");
    if let Some(existing) = slot.upgrade() {
        return Ok(existing);
    }
    let cluster = Arc::new(start_fluvio()?);
    *slot = Arc::downgrade(&cluster);
    Ok(cluster)
}

/// Start a full Fluvio local cluster (SC + SPU) using host networking, or attach
/// to one that is already listening.
///
/// The SC container is started first. Once the SC is ready the SPU spec is
/// registered via the admin API so the SC recognises the SPU when it connects.
/// The SPU process is then started via `docker exec` into the running container.
fn start_fluvio() -> anyhow::Result<FluvioCluster> {
    let endpoint = format!("127.0.0.1:{FLUVIO_SC_PORT}");

    // A cluster left running by a previous binary (or a concurrently-shutting-down
    // one) already owns the fixed host ports, so reuse it instead of racing it.
    if wait_for_port(&endpoint, std::time::Duration::from_millis(200)).is_ok() {
        register_spu(&endpoint)?;
        return Ok(FluvioCluster {
            container: None,
            endpoint,
        });
    }

    // Start the SC and wait for it to be ready.
    let container = GenericImage::new(FLUVIO_IMAGE, FLUVIO_TAG)
        .with_wait_for(WaitFor::message_on_stdout(
            "Streaming Controller started successfully",
        ))
        .with_cmd(vec![
            "/bin/sh",
            "-c",
            "/fluvio-run sc --local /tmp/fluvio & wait",
        ])
        .with_host_config_modifier(|hc| {
            hc.network_mode = Some("host".to_string());
        })
        .start()?;

    // The SC logs "started successfully" before its 9003 listener is bound.
    // With host networking and back-to-back test containers this race shows up
    // as ECONNREFUSED on the next admin call. Probe the port until accept() works.
    wait_for_port(&endpoint, std::time::Duration::from_secs(10))?;

    // Register the custom SPU with the SC before starting the SPU process.
    register_spu(&endpoint)?;

    // Start the SPU inside the already-running SC container.
    let spu_cmd = format!(
        "/fluvio-run spu \
           --id 5001 \
           --public-server 127.0.0.1:{FLUVIO_SPU_PORT} \
           --private-server 127.0.0.1:{FLUVIO_SPU_PRIVATE_PORT} \
           --sc-addr 127.0.0.1:{FLUVIO_SC_PRIVATE_PORT} \
           --log-base-dir /tmp/fluvio &"
    );
    container.exec(ExecCommand::new(["/bin/sh", "-c", &spu_cmd]))?;

    // Give the SPU time to complete registration with the SC.
    std::thread::sleep(std::time::Duration::from_secs(3));

    Ok(FluvioCluster {
        container: Some(container),
        endpoint,
    })
}

/// Block until `endpoint` accepts a TCP connection, or `timeout` elapses.
fn wait_for_port(endpoint: &str, timeout: std::time::Duration) -> anyhow::Result<()> {
    let addr: std::net::SocketAddr = endpoint
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid endpoint {endpoint}: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)) {
            Ok(_) => return Ok(()),
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "fluvio SC never accepted on {endpoint}: {e}"
                ));
            }
        }
    }
}

/// Register a custom SPU spec with the SC via the admin API.
///
/// Must be called after the SC is up but before the SPU process starts. Without
/// pre-registration the SC closes the SPU's connection immediately.
fn register_spu(endpoint: &str) -> anyhow::Result<()> {
    const SPU_ID: i32 = 5001;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let admin = FluvioAdmin::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio admin connect failed: {e}"))?;

        let spec = CustomSpuSpec {
            id: SPU_ID,
            public_endpoint: IngressPort::from_port_host(FLUVIO_SPU_PORT, "127.0.0.1".to_string()),
            private_endpoint: Endpoint {
                port: FLUVIO_SPU_PRIVATE_PORT,
                host: "127.0.0.1".to_string(),
                encryption: EncryptionEnum::PLAINTEXT,
            },
            rack: None,
            public_endpoint_local: None,
        };

        // Registering an SPU that is already known is success, not failure: the
        // cluster is shared (see `fluvio_cluster`), so whoever attaches second
        // finds `spu-5001` already defined and can proceed.
        match admin
            .create::<CustomSpuSpec>("spu-5001".to_string(), false, spec)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_already_exists(&e) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("SPU register failed: {e:?}")),
        }
    })
}

/// Fluvio reports "already defined" / "already exists" as an ordinary admin
/// error rather than a typed variant, so the check is on the rendered message.
/// Used to make cluster setup idempotent against the shared cluster.
fn is_already_exists<E: std::fmt::Debug>(err: &E) -> bool {
    let rendered = format!("{err:?}").to_lowercase();
    rendered.contains("already defined") || rendered.contains("already exists")
}

/// Create a topic using FluvioAdmin with a throwaway async runtime.
fn create_topic(endpoint: &str, topic: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let admin = FluvioAdmin::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio admin connect failed: {e}"))?;
        // Same reasoning as `register_spu`: the cluster outlives a single test
        // binary, so a re-run finds the topic already there.
        match admin
            .create::<TopicSpec>(
                topic.to_string(),
                false,
                TopicSpec::new_computed(1, 1, None),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_already_exists(&e) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("topic create failed: {e}")),
        }
    })
}

/// Seed records into a topic directly via the Fluvio producer.
fn seed_records(endpoint: &str, topic: &str, records: &[(&str, &[u8])]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let client = Fluvio::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio connect failed: {e}"))?;
        let producer = client
            .topic_producer(topic)
            .await
            .map_err(|e| anyhow::anyhow!("producer create failed: {e}"))?;
        for (key, value) in records {
            producer
                .send(*key, *value)
                .await
                .map_err(|e| anyhow::anyhow!("send failed: {e}"))?;
        }
        producer
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("flush failed: {e}"))?;
        Ok(())
    })
}

/// Read records from a topic directly via the Fluvio consumer.
/// Returns all records currently available (up to `limit`).
fn read_records(
    endpoint: &str,
    topic: &str,
    limit: usize,
) -> anyhow::Result<Vec<(Option<Vec<u8>>, Vec<u8>)>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let config = FluvioClusterConfig::new(endpoint);
        let client = Fluvio::connect_with_config(&config)
            .await
            .map_err(|e| anyhow::anyhow!("fluvio connect failed: {e}"))?;
        let consumer_config = ConsumerConfigExt::builder()
            .topic(topic)
            .partition(0u32)
            .offset_start(Offset::beginning())
            .build()
            .map_err(|e| anyhow::anyhow!("consumer config failed: {e}"))?;
        let mut stream = client
            .consumer_with_config(consumer_config)
            .await
            .map_err(|e| anyhow::anyhow!("consumer create failed: {e}"))?;
        let mut results = Vec::new();
        while results.len() < limit {
            match stream.next().await {
                Some(Ok(record)) => {
                    results.push((record.key().map(|k| k.to_vec()), record.value().to_vec()));
                }
                Some(Err(e)) => {
                    return Err(anyhow::anyhow!("record error: {e:?}"));
                }
                None => break,
            }
        }
        Ok(results)
    })
}

// ---- Tests ----

/// Error propagates correctly when the Fluvio SC is not reachable.
///
/// Needs no container, so it runs first (the step-10 convention).
#[test]
fn connection_refused() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _events = fluvio_sub(
        &g,
        RunMode::RealTime,
        FluvioConnection::new("127.0.0.1:59999"),
        "any-topic",
        0,
        None,
    )
    .expect("realtime fluvio_sub wires without error")
    .collapse_accumulate();

    let result = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(result.is_err(), "expected connection error");
}

/// Records pre-seeded before the consumer starts are received from offset 0.
#[test]
fn sub_from_beginning() -> anyhow::Result<()> {
    let cluster = fluvio_cluster()?;
    let endpoint = cluster.endpoint.clone();
    let topic = "sub-from-beginning";
    create_topic(&endpoint, topic)?;
    seed_records(&endpoint, topic, &[("k1", b"hello"), ("k2", b"world")])?;

    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let collected = fluvio_sub(
        &g,
        RunMode::RealTime,
        FluvioConnection::new(&endpoint),
        topic,
        0,
        None,
    )?
    .collapse_accumulate();
    // RunFor::Cycles(2): the two seeded records may arrive in one burst or two;
    // `collapse_accumulate` flattens either shape losslessly.
    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Cycles(2))?;

    let events: Vec<FluvioEvent> = runner.value(&collected);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value, b"hello");
    assert_eq!(events[1].value, b"world");
    Ok(())
}

/// `fluvio_pub` writes records; verify via a direct consumer read.
#[test]
fn pub_round_trip() -> anyhow::Result<()> {
    let cluster = fluvio_cluster()?;
    let endpoint = cluster.endpoint.clone();
    let topic = "pub-round-trip";
    create_topic(&endpoint, topic)?;

    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .constant(burst![
            FluvioRecord::with_key("greeting", b"hello".to_vec()),
            FluvioRecord::new(b"world".to_vec()),
        ])
        .fluvio_pub(FluvioConnection::new(&endpoint), topic, None)?;
    g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;

    let records = read_records(&endpoint, topic, 2)?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].1, b"hello");
    assert_eq!(records[1].1, b"world");
    Ok(())
}

/// Records produced after the consumer starts are received.
#[test]
fn sub_live_stream() -> anyhow::Result<()> {
    let cluster = fluvio_cluster()?;
    let endpoint = cluster.endpoint.clone();
    let topic = "sub-live-stream";
    create_topic(&endpoint, topic)?;

    let endpoint_clone = endpoint.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        seed_records(&endpoint_clone, topic, &[("live", b"event")])
            .expect("seeding the live record");
    });

    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let collected = fluvio_sub(
        &g,
        RunMode::RealTime,
        FluvioConnection::new(&endpoint),
        topic,
        0,
        None,
    )?
    .collapse_accumulate();
    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Cycles(1))?;
    handle.join().expect("seed thread");

    let events = runner.value(&collected);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value, b"event");
    Ok(())
}

/// Keyless records ([`RecordKey::NULL`]) are written and read back correctly.
#[test]
fn pub_keyless_record() -> anyhow::Result<()> {
    let cluster = fluvio_cluster()?;
    let endpoint = cluster.endpoint.clone();
    let topic = "keyless-records";
    create_topic(&endpoint, topic)?;

    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let _sink = g
        .constant(burst![FluvioRecord::new(b"no-key".to_vec())])
        .fluvio_pub(FluvioConnection::new(&endpoint), topic, None)?;
    g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;

    let records = read_records(&endpoint, topic, 1)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, None, "key should be None for keyless record");
    assert_eq!(records[0].1, b"no-key");
    Ok(())
}

/// String keys are stored and readable from the consumer.
#[test]
fn pub_keyed_record() -> anyhow::Result<()> {
    let cluster = fluvio_cluster()?;
    let endpoint = cluster.endpoint.clone();
    let topic = "keyed-records";
    create_topic(&endpoint, topic)?;

    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    // The single-record convenience impl (no manual burst wrapping).
    let _sink = g
        .constant(FluvioRecord::with_key("my-key", b"my-value".to_vec()))
        .fluvio_pub(FluvioConnection::new(&endpoint), topic, None)?;
    g.build().run(RunMode::RealTime, RunFor::Cycles(1))?;

    let records = read_records(&endpoint, topic, 1)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0.as_deref(), Some(b"my-key".as_ref()));
    assert_eq!(records[0].1, b"my-value");
    Ok(())
}

/// A consumer with a non-zero `start_offset` skips earlier records.
#[test]
fn sub_from_absolute_offset() -> anyhow::Result<()> {
    let cluster = fluvio_cluster()?;
    let endpoint = cluster.endpoint.clone();
    let topic = "sub-abs-offset";
    create_topic(&endpoint, topic)?;
    seed_records(
        &endpoint,
        topic,
        &[("k0", b"first"), ("k1", b"second"), ("k2", b"third")],
    )?;

    // Start from offset 1 — should receive "second" and "third" only.
    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    let collected = fluvio_sub(
        &g,
        RunMode::RealTime,
        FluvioConnection::new(&endpoint),
        topic,
        0,
        Some(1),
    )?
    .collapse_accumulate();
    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Cycles(2))?;

    let events: Vec<FluvioEvent> = runner.value(&collected);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value, b"second");
    assert_eq!(events[1].value, b"third");
    assert_eq!(events[0].offset, 1, "offsets are absolute");
    Ok(())
}
