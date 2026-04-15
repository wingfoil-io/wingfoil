//! Integration tests for the `web` adapter.
//!
//! These tests spin up an in-process [`WebServer`] and connect a
//! `tokio-tungstenite` client to it. No external service is required.
//!
//! Run with:
//!
//! ```sh
//! cargo test --features web-integration-test -p wingfoil \
//!   -- --test-threads=1 adapters::web::integration_tests
//! ```

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::*;
use crate::nodes::{NodeOperators, StreamOperators, constant, ticker};
use crate::types::*;
use crate::{RunFor, RunMode};

type TungsteniteStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UiClick {
    button: String,
    count: u32,
}

async fn connect(port: u16) -> anyhow::Result<TungsteniteStream> {
    let url = format!("ws://127.0.0.1:{port}/ws");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((mut socket, _)) => {
                // Drain the Hello frame so tests don't accidentally decode it.
                let _ = socket.next().await;
                return Ok(socket);
            }
            Err(e) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
                let _ = e;
            }
            Err(e) => return Err(anyhow::anyhow!("connect: {e}")),
        }
    }
}

async fn send_control(
    socket: &mut TungsteniteStream,
    codec: CodecKind,
    ctrl: ControlMessage,
) -> anyhow::Result<()> {
    let payload = codec.encode(&ctrl)?;
    let env = Envelope {
        topic: CONTROL_TOPIC.to_string(),
        time_ns: 0,
        payload,
    };
    let bytes = codec.encode(&env)?;
    socket.send(WsMessage::Binary(bytes)).await?;
    Ok(())
}

async fn send_payload<T: Serialize>(
    socket: &mut TungsteniteStream,
    codec: CodecKind,
    topic: &str,
    value: &T,
) -> anyhow::Result<()> {
    let payload = codec.encode(value)?;
    let env = Envelope {
        topic: topic.to_string(),
        time_ns: 0,
        payload,
    };
    let bytes = codec.encode(&env)?;
    socket.send(WsMessage::Binary(bytes)).await?;
    Ok(())
}

async fn recv_envelope(
    socket: &mut TungsteniteStream,
    codec: CodecKind,
) -> anyhow::Result<Envelope> {
    loop {
        let msg = socket
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("socket closed"))??;
        match msg {
            WsMessage::Binary(bytes) => return codec.decode(&bytes),
            WsMessage::Text(t) => return codec.decode(t.as_bytes()),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(_) => anyhow::bail!("socket closed"),
            _ => continue,
        }
    }
}

/// Spawn a client on its own thread + tokio runtime that subscribes to
/// `topic` and captures up to `max_frames` envelopes. Returns a handle
/// whose `join()` produces the collected envelopes.
fn spawn_subscriber(
    port: u16,
    topic: &str,
    codec: CodecKind,
    max_frames: usize,
    ready: std::sync::mpsc::Sender<()>,
) -> std::thread::JoinHandle<anyhow::Result<Vec<Envelope>>> {
    let topic = topic.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut socket = connect(port).await?;
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec![topic.clone()],
                },
            )
            .await?;
            ready.send(()).ok();

            let mut out = Vec::with_capacity(max_frames);
            let overall_deadline = Instant::now() + Duration::from_secs(10);
            while out.len() < max_frames && Instant::now() < overall_deadline {
                match tokio::time::timeout(
                    Duration::from_secs(2),
                    recv_envelope(&mut socket, codec),
                )
                .await
                {
                    Ok(Ok(env)) if env.topic == topic => out.push(env),
                    Ok(Ok(_)) => continue, // ignore other topics (hello, etc.)
                    Ok(Err(_)) | Err(_) => break,
                }
            }
            Ok(out)
        })
    })
}

#[test]
fn test_bind_port_zero() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    assert!(server.port() > 0, "expected OS to assign a real port");
    drop(server);
    Ok(())
}

#[test]
fn test_bind_fails_when_port_occupied() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();
    let result = WebServer::bind(format!("127.0.0.1:{port}")).start();
    assert!(result.is_err(), "expected bind error when port occupied");
}

#[test]
fn test_pub_round_trip_bincode() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let client = spawn_subscriber(port, "tick", codec, 5, ready_tx);
    // Wait for the client to subscribe before starting the graph so the
    // initial frames aren't lost.
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("client failed to subscribe");

    let counter = ticker(Duration::from_millis(10)).count();
    counter.web_pub(&server, "tick").run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(500)),
    )?;

    let envs = client.join().expect("client thread panic")?;
    assert!(!envs.is_empty(), "no envelopes received");
    let mut last = 0u64;
    for env in &envs {
        assert_eq!(env.topic, "tick");
        assert!(env.time_ns >= last, "time_ns should be monotonic");
        last = env.time_ns;
        let value: u64 = codec.decode(&env.payload)?;
        assert!(value >= 1);
    }
    Ok(())
}

#[test]
fn test_sub_round_trip_bincode() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let clicks: Rc<dyn Stream<Burst<UiClick>>> = web_sub(&server, "ui_events");
    let collected = clicks.collect();

    let client_handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut socket = connect(port).await?;
            for i in 0..3u32 {
                send_payload(
                    &mut socket,
                    codec,
                    "ui_events",
                    &UiClick {
                        button: "ok".to_string(),
                        count: i,
                    },
                )
                .await?;
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            anyhow::Ok(())
        })
    });

    collected
        .clone()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
    client_handle.join().unwrap()?;

    let all: Vec<UiClick> = collected
        .peek_value()
        .into_iter()
        .flat_map(|v| v.value.into_iter())
        .collect();
    assert!(
        all.len() >= 3,
        "expected at least 3 clicks, got {}",
        all.len()
    );
    for (i, click) in all.iter().take(3).enumerate() {
        assert_eq!(click.button, "ok");
        assert_eq!(click.count, i as u32);
    }
    Ok(())
}

#[test]
fn test_pub_round_trip_json() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0")
        .codec(CodecKind::Json)
        .start()?;
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let client = spawn_subscriber(port, "answer", codec, 1, ready_tx);
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("client failed to subscribe");

    // Run on a ticker so the `constant` value ticks at least once after the
    // subscriber is connected.
    ticker(Duration::from_millis(20))
        .count()
        .map(|_| 42u32)
        .web_pub(&server, "answer")
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(300)),
        )?;

    let envs = client.join().expect("client thread panic")?;
    assert!(!envs.is_empty(), "no envelopes received");
    let env = &envs[0];
    assert_eq!(env.topic, "answer");
    let decoded: u32 = codec.decode(&env.payload)?;
    assert_eq!(decoded, 42);
    Ok(())
}

#[test]
fn test_historical_mode_is_noop() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start_historical()?;
    assert!(server.is_historical_noop());
    assert_eq!(server.port(), 0);

    let pub_node = constant(7u32).web_pub(&server, "x");
    pub_node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))?;

    let sub_stream: Rc<dyn Stream<Burst<u32>>> = web_sub(&server, "y");
    let collected = sub_stream.collect();
    collected
        .clone()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))?;
    assert!(
        collected.peek_value().is_empty(),
        "historical sub should not tick"
    );
    Ok(())
}

#[test]
fn test_multiple_subscribers_receive_same_frames() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let (a_tx, a_rx) = std::sync::mpsc::channel();
    let (b_tx, b_rx) = std::sync::mpsc::channel();
    let a_client = spawn_subscriber(port, "bcast", codec, 3, a_tx);
    let b_client = spawn_subscriber(port, "bcast", codec, 3, b_tx);
    a_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    b_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    ticker(Duration::from_millis(10))
        .count()
        .web_pub(&server, "bcast")
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(500)),
        )?;

    let a_envs = a_client.join().unwrap()?;
    let b_envs = b_client.join().unwrap()?;
    assert!(!a_envs.is_empty());
    assert!(!b_envs.is_empty());
    assert!(a_envs.iter().all(|e| e.topic == "bcast"));
    assert!(b_envs.iter().all(|e| e.topic == "bcast"));
    Ok(())
}

#[test]
fn test_bad_envelope_is_ignored_not_fatal() -> anyhow::Result<()> {
    // Server must tolerate garbage from clients without panicking or closing
    // other subscriptions.
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let received = Arc::new(Mutex::new(Vec::<Envelope>::new()));
    let received_clone = received.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let mut socket = connect(port).await.unwrap();
            // Send garbage that is not a valid envelope.
            socket
                .send(WsMessage::Binary(vec![0xFFu8; 8]))
                .await
                .unwrap();
            // Subscribe; this must still work despite the garbage earlier.
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec!["safe".to_string()],
                },
            )
            .await
            .unwrap();
            ready_tx.send(()).ok();
            while let Ok(Ok(env)) = tokio::time::timeout(
                Duration::from_millis(500),
                recv_envelope(&mut socket, codec),
            )
            .await
            {
                if env.topic == "safe" {
                    received_clone.lock().unwrap().push(env);
                    break;
                }
            }
        });
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    ticker(Duration::from_millis(10))
        .count()
        .map(|_| 1u32)
        .web_pub(&server, "safe")
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(300)),
        )?;

    client.join().unwrap();
    let got = received.lock().unwrap().clone();
    assert!(!got.is_empty(), "expected at least one safe frame");
    assert_eq!(got[0].topic, "safe");
    Ok(())
}
