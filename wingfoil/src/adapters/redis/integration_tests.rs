//! Integration tests for the Redis adapter.
//!
//! Requires Docker. Run with:
//! ```sh
//! cargo test --features redis-integration-test -p wingfoil \
//!   -- --test-threads=1 redis::integration_tests
//! ```

use super::*;
use crate::nodes::{NodeOperators, StreamOperators, constant};
use crate::types::Burst;
use crate::{RunFor, RunMode, burst};
use futures::StreamExt;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use testcontainers::{GenericImage, core::WaitFor, runners::SyncRunner};

const REDIS_PORT: u16 = 6379;
const REDIS_IMAGE: &str = "redis";
const REDIS_TAG: &str = "7-alpine";

/// Start a Redis container and return the connection URL.
/// The returned container must be kept alive for the duration of the test.
fn start_redis() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new(REDIS_IMAGE, REDIS_TAG)
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .start()?;
    let port = container.get_host_port_ipv4(REDIS_PORT)?;
    let url = format!("redis://127.0.0.1:{port}");
    Ok((container, url))
}

/// Spawn a background thread that repeatedly publishes `payload` to `channel`
/// until the returned flag is set. Redis Pub/Sub drops messages with no live
/// subscriber, so the publisher keeps retrying until the graph has captured one.
fn spawn_publisher(
    url: &str,
    channel: &str,
    payload: &[u8],
) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let done = Arc::new(AtomicBool::new(false));
    let done_thread = done.clone();
    let url = url.to_string();
    let channel = channel.to_string();
    let payload = payload.to_vec();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let client = redis::Client::open(url.as_str()).unwrap();
            let mut conn = client.get_multiplexed_async_connection().await.unwrap();
            while !done_thread.load(Ordering::Relaxed) {
                let _: i64 = redis::cmd("PUBLISH")
                    .arg(channel.as_str())
                    .arg(payload.as_slice())
                    .query_async(&mut conn)
                    .await
                    .unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
    });
    (done, handle)
}

/// Subscribe to `channel` on a dedicated thread, set `ready` once the subscription
/// is registered server-side, then collect up to `n` payloads (with a timeout).
fn spawn_subscriber(
    url: &str,
    channel: &str,
    n: usize,
    ready: Arc<AtomicBool>,
) -> std::thread::JoinHandle<anyhow::Result<Vec<Vec<u8>>>> {
    let url = url.to_string();
    let channel = channel.to_string();
    std::thread::spawn(move || -> anyhow::Result<Vec<Vec<u8>>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let client = redis::Client::open(url.as_str())?;
            let mut pubsub = client.get_async_pubsub().await?;
            pubsub.subscribe(channel.as_str()).await?;
            // Subscription is now active server-side.
            ready.store(true, Ordering::Relaxed);
            let mut messages = pubsub.on_message();
            let mut out = Vec::new();
            while out.len() < n {
                match tokio::time::timeout(Duration::from_secs(10), messages.next()).await {
                    Ok(Some(msg)) => out.push(msg.get_payload_bytes().to_vec()),
                    _ => break,
                }
            }
            Ok(out)
        })
    })
}

// ---- Tests ----

#[test]
fn test_connection_refused() {
    // Error propagates correctly without a running Redis.
    let conn = RedisConnection::new("redis://127.0.0.1:59999");
    let result = redis_sub(conn, "ch")
        .collapse()
        .collect()
        .run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(result.is_err(), "expected connection error");
}

#[test]
fn test_sub_live_message() -> anyhow::Result<()> {
    // A message published while the graph is subscribed arrives as a RedisEvent.
    let (_container, url) = start_redis()?;
    let conn = RedisConnection::new(&url);

    let (done, publisher) = spawn_publisher(&url, "live", b"hello");

    let collected = redis_sub(conn, "live").collapse().collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;

    done.store(true, Ordering::Relaxed);
    publisher.join().unwrap();

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value.channel, "live");
    assert_eq!(events[0].value.payload, b"hello");
    Ok(())
}

#[test]
fn test_pub_round_trip() -> anyhow::Result<()> {
    // redis_pub publishes a message; a direct subscriber receives it.
    let (_container, url) = start_redis()?;
    let conn = RedisConnection::new(&url);

    let ready = Arc::new(AtomicBool::new(false));
    let subscriber = spawn_subscriber(&url, "rt", 1, ready.clone());

    // Wait until the subscriber's SUBSCRIBE has been registered server-side so the
    // fire-and-forget PUBLISH is guaranteed to reach it.
    while !ready.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }

    let source = constant(burst![RedisEntry {
        channel: "rt".to_string(),
        payload: b"value1".to_vec(),
    }]);
    redis_pub(conn, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let payloads = subscriber.join().unwrap()?;
    assert_eq!(payloads, vec![b"value1".to_vec()]);
    Ok(())
}

#[test]
fn test_pub_multiple_entries_in_burst() -> anyhow::Result<()> {
    // Multiple entries in a single burst are all published.
    let (_container, url) = start_redis()?;
    let conn = RedisConnection::new(&url);

    let ready = Arc::new(AtomicBool::new(false));
    let subscriber = spawn_subscriber(&url, "multi", 3, ready.clone());

    while !ready.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }

    let source: Rc<dyn crate::Stream<Burst<RedisEntry>>> = constant(burst![
        RedisEntry {
            channel: "multi".to_string(),
            payload: b"a".to_vec()
        },
        RedisEntry {
            channel: "multi".to_string(),
            payload: b"b".to_vec()
        },
        RedisEntry {
            channel: "multi".to_string(),
            payload: b"c".to_vec()
        },
    ]);
    redis_pub(conn, &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let mut payloads = subscriber.join().unwrap()?;
    payloads.sort();
    assert_eq!(payloads, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    Ok(())
}

#[test]
fn test_redis_entry_payload_str() {
    let entry = RedisEntry {
        channel: "ch".to_string(),
        payload: b"bar".to_vec(),
    };
    assert_eq!(entry.payload_str().unwrap(), "bar");
}

// ---- Streams ----

/// Append a single-field entry directly to a stream via the client.
fn xadd(url: &str, key: &str, field: &str, value: &[u8]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = redis::Client::open(url)?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        redis::cmd("XADD")
            .arg(key)
            .arg("*")
            .arg(field)
            .arg(value)
            .query_async::<String>(&mut conn)
            .await?;
        Ok(())
    })
}

#[test]
fn test_stream_write_and_read_snapshot() -> anyhow::Result<()> {
    // redis_stream_write appends entries; redis_stream_read replays them as a snapshot.
    let (_container, url) = start_redis()?;
    let conn = RedisConnection::new(&url);

    let source: Rc<dyn crate::Stream<Burst<RedisStreamRecord>>> = constant(burst![
        RedisStreamRecord::single("events", "kind", b"login".to_vec()),
        RedisStreamRecord::single("events", "kind", b"logout".to_vec()),
    ]);
    redis_stream_write(conn.clone(), &source).run(RunMode::RealTime, RunFor::Cycles(1))?;

    let collected = redis_stream_read(conn, "events").collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;

    let events: Vec<RedisStreamEvent> = collected
        .peek_value()
        .into_iter()
        .flat_map(|v| v.value.into_iter())
        .collect();
    assert_eq!(events.len(), 2);
    let kinds: Vec<Vec<u8>> = events
        .iter()
        .map(|e| e.field("kind").unwrap_or_default().to_vec())
        .collect();
    assert_eq!(kinds, vec![b"login".to_vec(), b"logout".to_vec()]);
    // Redis assigns monotonically increasing IDs.
    assert!(events[0].id < events[1].id);
    Ok(())
}

#[test]
fn test_stream_read_live_tail() -> anyhow::Result<()> {
    // An entry appended after the (empty) snapshot arrives via the XREAD tail.
    let (_container, url) = start_redis()?;
    let conn = RedisConnection::new(&url);

    let url_writer = url.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        xadd(&url_writer, "tail", "msg", b"live").unwrap();
    });

    let collected = redis_stream_read(conn, "tail").collapse().collect();
    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Cycles(1))?;
    writer.join().unwrap();

    let events = collected.peek_value();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value.key, "tail");
    assert_eq!(events[0].value.field("msg"), Some(b"live".as_ref()));
    Ok(())
}

#[test]
fn test_stream_record_single() {
    let record = RedisStreamRecord::single("k", "f", b"v".to_vec());
    assert_eq!(record.key, "k");
    assert_eq!(record.fields, vec![("f".to_string(), b"v".to_vec())]);
}
