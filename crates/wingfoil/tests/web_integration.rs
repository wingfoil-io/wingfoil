//! Socket-level integration tests for the `web` adapter — a parity port of the
//! legacy `legacy/wingfoil/src/adapters/web/integration_tests.rs`.
//!
//! These spin up an in-process [`WebServer`] and connect a real
//! `tokio-tungstenite` client to it over loopback. No external service or
//! container is required, but they are still **tier 2**: they bind sockets,
//! spawn client threads, and wait on frames with multi-second deadlines, which
//! is too slow and too load-sensitive for the ordinary test job.
//!
//! That is why they live in a `*_integration.rs` binary. CI's `test` job runs
//! `cargo nextest … --all-features -E 'not binary(/_integration$/)'`
//! (`.github/workflows/rust-test.yml`), so the **filename suffix** is the only
//! thing keeping them out of it — they run in `web-integration.yml` instead.
//! Wiring-level tests that need nothing listening stay in `web_adapter.rs`.
//!
//! ```sh
//! cargo test -p wingfoil \
//!     --features web-integration-test -- --test-threads=1
//! ```
//!
//! The TLS round trip additionally needs `--features web-tls-integration-test`
//! (it pulls in the rustls client stack plus the `rcgen` cert fixture); that
//! feature implies this one, so the command above with `-tls-` runs everything.
#![cfg(feature = "web-integration-test")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use wingfoil::adapters::web::{
    CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WebBurstSinkOps, WebServer, WebSinkOps,
    web_sub,
};
use wingfoil::async_source::{RunParams, produce_async};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

type TungsteniteStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UiClick {
    button: String,
    count: u32,
}

/// How long a single `recv` waits before the client loops go round again.
///
/// Exceeding it means no frame has arrived *yet*, not that the stream ended.
/// The loops below must therefore retry rather than break: a loaded CI runner
/// can take longer than this to deliver the first frame, and treating that as
/// end-of-stream makes the test assert against an empty vec.
const RECV_TIMEOUT: Duration = Duration::from_secs(2);

/// The real bound on a client thread waiting for the frames a test expects.
///
/// Kept under nextest's 10s `slow-timeout` (`.config/nextest.toml`), which
/// *terminates* a test rather than letting it fail. A deadline at or above that
/// turns a genuine failure into a kill with no assertion message.
const CLIENT_DEADLINE: Duration = Duration::from_secs(8);

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

async fn send_control(
    socket: &mut TungsteniteStream,
    codec: CodecKind,
    ctrl: ControlMessage,
) -> anyhow::Result<()> {
    send_payload(socket, codec, CONTROL_TOPIC, &ctrl).await
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

/// Block until a publish on `topic` would actually reach the client.
///
/// The `ready` channel every client here sends on only says it *wrote* its
/// `Subscribe` frame; the server registers that asynchronously on the
/// connection's reader task. A `broadcast` channel drops anything published
/// before the receiver exists, so a test that starts its graph on the `ready`
/// signal alone can lose the whole sequence — which is exactly what it looks
/// like when `burst_stream_publishes_atomic_arrays` asserts against `[]`.
///
/// Tests publishing over a long window (the ticker ones) survive that race by
/// accident; the ones publishing a short finite sequence do not. Wait here
/// instead of sleeping a guessed interval.
fn wait_for_subscribers(server: &WebServer, topic: &str, count: usize) {
    let deadline = Instant::now() + CLIENT_DEADLINE;
    while Instant::now() < deadline {
        if server.subscriber_count(topic) >= count {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "server registered {} of {count} subscribers for `{topic}` within {CLIENT_DEADLINE:?}",
        server.subscriber_count(topic),
    );
}

/// Spawn a client on its own thread + tokio runtime that subscribes to `topic`
/// and captures up to `max_frames` envelopes. Returns a handle whose `join()`
/// produces the collected envelopes.
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
            let overall_deadline = Instant::now() + CLIENT_DEADLINE;
            while out.len() < max_frames && Instant::now() < overall_deadline {
                match tokio::time::timeout(RECV_TIMEOUT, recv_envelope(&mut socket, codec)).await {
                    Ok(Ok(env)) if env.topic == topic => out.push(env),
                    Ok(Ok(_)) => continue, // ignore other topics (hello, etc.)
                    Ok(Err(_)) => break,
                    // Nothing yet — keep waiting until the outer deadline.
                    Err(_) => continue,
                }
            }
            Ok(out)
        })
    })
}

#[test]
fn pub_round_trip_bincode() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let client = spawn_subscriber(port, "tick", codec, 5, ready_tx);
    // Wait for the client to subscribe before starting the graph so the initial
    // frames aren't lost.
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("client failed to subscribe");
    wait_for_subscribers(&server, "tick", 1);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .web_pub(&server, "tick")?;
    g.build().run(
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
        // A scalar upstream serializes as a scalar payload; the client treats it
        // as a one-element burst.
        let value: u64 = codec.decode(&env.payload)?;
        assert!(value >= 1);
    }
    Ok(())
}

#[test]
fn sub_round_trip_bincode() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let g = GraphBuilder::new();
    let collected = web_sub::<UiClick>(&g, &server, "ui_events")?.collapse_accumulate();

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

    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
    client_handle.join().expect("client thread panic")?;

    let all = runner.value(&collected);
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
fn pub_round_trip_json() -> anyhow::Result<()> {
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
    wait_for_subscribers(&server, "answer", 1);

    // Run on a ticker so the constant value ticks at least once after the
    // subscriber is connected.
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(20))
        .count()
        .map(|_: &u64| 42u32)
        .web_pub(&server, "answer")?;
    g.build().run(
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

/// A historical-mode graph served through a real (`start()`) server must stream
/// every value to a connected client in order, then emit a `Complete` control
/// frame when the replay ends. This is the core "stream a backtest / slow
/// computation to the browser" path, and the `Complete` marker is what
/// `@wingfoil/client`'s `onComplete` (and its stop-reconnecting logic) hinges on.
#[test]
fn historical_streaming_completes() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    assert!(!server.is_historical_noop());
    let port = server.port();
    let codec = server.codec();

    // Client thread: subscribe, then collect topic frames until a
    // `Complete { topic }` control frame arrives (or timeout).
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || -> anyhow::Result<(Vec<u64>, bool)> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut socket = connect(port).await?;
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec!["hist".to_string()],
                },
            )
            .await?;
            ready_tx.send(()).ok();

            let mut values = Vec::new();
            let mut completed = false;
            let deadline = Instant::now() + CLIENT_DEADLINE;
            while Instant::now() < deadline {
                let env =
                    match tokio::time::timeout(RECV_TIMEOUT, recv_envelope(&mut socket, codec))
                        .await
                    {
                        Ok(Ok(env)) => env,
                        Ok(Err(_)) => break,
                        // Nothing yet — keep waiting until the outer deadline.
                        Err(_) => continue,
                    };
                if env.topic == "hist" {
                    values.push(codec.decode::<u64>(&env.payload)?);
                } else if env.topic == CONTROL_TOPIC
                    && let ControlMessage::Complete { topic } = codec.decode(&env.payload)?
                {
                    assert_eq!(topic, "hist");
                    completed = true;
                    break;
                }
            }
            Ok((values, completed))
        })
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("client failed to subscribe");
    wait_for_subscribers(&server, "hist", 1);

    // 50 values, well under the broadcast buffer, so a loopback client receives
    // every one with no loss.
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .web_pub(&server, "hist")?;
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(50))?;

    let (values, completed) = handle.join().expect("client thread panic")?;
    assert!(
        completed,
        "expected a Complete control frame at end of replay"
    );
    assert_eq!(
        values,
        (1..=50).collect::<Vec<u64>>(),
        "every historical frame should arrive in order"
    );
    Ok(())
}

/// Publishing a `Stream<Burst<T>>` puts the whole same-`time_ns` group on the
/// wire as one array frame — atomic, so a lossy drop can never split a
/// timestamp. The client sees `[1, 2]` at t=100 and `[3]` at t=200.
#[test]
fn burst_stream_publishes_atomic_arrays() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    // Client: collect each "burst" frame's decoded array until Complete.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || -> anyhow::Result<Vec<Vec<u32>>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut socket = connect(port).await?;
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec!["burst".to_string()],
                },
            )
            .await?;
            ready_tx.send(()).ok();

            let mut frames: Vec<Vec<u32>> = Vec::new();
            let deadline = Instant::now() + CLIENT_DEADLINE;
            while Instant::now() < deadline {
                let env =
                    match tokio::time::timeout(RECV_TIMEOUT, recv_envelope(&mut socket, codec))
                        .await
                    {
                        Ok(Ok(env)) => env,
                        Ok(Err(_)) => break,
                        // Nothing yet — keep waiting until the outer deadline.
                        Err(_) => continue,
                    };
                if env.topic == "burst" {
                    // A same-time burst decodes to a whole array in one frame.
                    frames.push(codec.decode::<Vec<u32>>(&env.payload)?);
                } else if env.topic == CONTROL_TOPIC
                    && matches!(codec.decode(&env.payload)?, ControlMessage::Complete { .. })
                {
                    break;
                }
            }
            Ok(frames)
        })
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("client failed to subscribe");
    wait_for_subscribers(&server, "burst", 1);

    // Two values at t=100 (grouped into one Burst) and one at t=200. The burst
    // sink converts each `Burst<T>` to the wire array itself.
    let g = GraphBuilder::new();
    let bursts = produce_async(
        &g,
        |_p: RunParams| async move {
            Ok(async_stream::stream! {
                yield Ok((NanoTime::new(100), 1u32));
                yield Ok((NanoTime::new(100), 2u32));
                yield Ok((NanoTime::new(200), 3u32));
            })
        },
        None,
    )?;
    let _sink = bursts.web_pub_bursts(&server, "burst")?;
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    let frames = handle.join().expect("client thread panic")?;
    assert_eq!(
        frames,
        vec![vec![1u32, 2], vec![3]],
        "same-time values must arrive as one atomic array frame"
    );
    Ok(())
}

#[test]
fn multiple_subscribers_receive_same_frames() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let (a_tx, a_rx) = std::sync::mpsc::channel();
    let (b_tx, b_rx) = std::sync::mpsc::channel();
    let a_client = spawn_subscriber(port, "bcast", codec, 3, a_tx);
    let b_client = spawn_subscriber(port, "bcast", codec, 3, b_tx);
    a_rx.recv_timeout(Duration::from_secs(5))?;
    b_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "bcast", 2);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .web_pub(&server, "bcast")?;
    g.build().run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(500)),
    )?;

    let a_envs = a_client.join().expect("client thread panic")?;
    let b_envs = b_client.join().expect("client thread panic")?;
    assert!(!a_envs.is_empty());
    assert!(!b_envs.is_empty());
    assert!(a_envs.iter().all(|e| e.topic == "bcast"));
    assert!(b_envs.iter().all(|e| e.topic == "bcast"));
    Ok(())
}

#[test]
fn bad_envelope_is_ignored_not_fatal() -> anyhow::Result<()> {
    // The server must tolerate garbage from clients without panicking or closing
    // other subscriptions.
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let received = Arc::new(Mutex::new(Vec::<Envelope>::new()));
    let received_clone = received.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut socket = connect(port).await?;
            // Send garbage that is not a valid envelope.
            socket.send(WsMessage::Binary(vec![0xFFu8; 8])).await?;
            // Subscribe; this must still work despite the garbage earlier.
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec!["safe".to_string()],
                },
            )
            .await?;
            ready_tx.send(()).ok();
            while let Ok(Ok(env)) = tokio::time::timeout(
                Duration::from_millis(500),
                recv_envelope(&mut socket, codec),
            )
            .await
            {
                if env.topic == "safe" {
                    received_clone
                        .lock()
                        .expect("received mutex poisoned")
                        .push(env);
                    break;
                }
            }
            anyhow::Ok(())
        })
    });
    ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "safe", 1);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|_: &u64| 1u32)
        .web_pub(&server, "safe")?;
    g.build().run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(300)),
    )?;

    client.join().expect("client thread panic")?;
    let got = received.lock().expect("received mutex poisoned").clone();
    assert!(!got.is_empty(), "expected at least one safe frame");
    assert_eq!(got[0].topic, "safe");
    Ok(())
}

/// Round-trip a publish through a `wss://` (rustls) connection. Generates a
/// fresh self-signed cert with rcgen, hands the PEM to the server's `.tls()`
/// builder, and uses tokio-tungstenite's rustls connector with the cert
/// installed in its trust store on the client side.
///
/// Gated behind `web-tls-integration-test` rather than plain `web-tls` because
/// it pulls in `tokio-tungstenite/rustls-tls-webpki-roots` (and its transitive
/// deps) for the client.
#[cfg(feature = "web-tls-integration-test")]
#[test]
fn pub_round_trip_tls() -> anyhow::Result<()> {
    use std::io::Write as _;

    use rustls::RootCertStore;
    use rustls::pki_types::CertificateDer;
    use tokio_tungstenite::Connector;

    // Self-signed cert valid for `localhost` so the client SNI/SAN check passes
    // without rcgen-side surgery.
    let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_pem = issued.cert.pem();
    let key_pem = issued.key_pair.serialize_pem();
    let cert_der: CertificateDer<'static> = issued.cert.der().clone();

    // std::env::temp_dir() — no need for the `tempfile` crate just to hold two
    // PEM files for one test. pid + nanos dodges collisions between concurrent
    // test runs on the same box.
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let cert_path = std::env::temp_dir().join(format!("wingfoil-web-tls-{unique}-cert.pem"));
    let key_path = std::env::temp_dir().join(format!("wingfoil-web-tls-{unique}-key.pem"));
    std::fs::File::create(&cert_path)?.write_all(cert_pem.as_bytes())?;
    std::fs::File::create(&key_path)?.write_all(key_pem.as_bytes())?;
    scopeguard::defer! {
        let _ = std::fs::remove_file(&cert_path);
        let _ = std::fs::remove_file(&key_path);
    }

    let server = WebServer::bind("127.0.0.1:0")
        .codec(CodecKind::Json)
        .tls(&cert_path, &key_path)
        .start()?;
    assert!(server.is_tls());
    let port = server.port();
    let codec = server.codec();

    let mut roots = RootCertStore::empty();
    roots.add(cert_der)?;
    let client_config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = Connector::Rustls(Arc::new(client_config));

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let topic = "tls_tick".to_string();
    let topic_thread = topic.clone();
    let client = std::thread::spawn(move || -> anyhow::Result<Vec<Envelope>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let url = format!("wss://localhost:{port}/ws");
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut socket, _) = loop {
                match tokio_tungstenite::connect_async_tls_with_config(
                    url.as_str(),
                    None,
                    false,
                    Some(connector.clone()),
                )
                .await
                {
                    Ok(s) => break s,
                    Err(e) if Instant::now() < deadline => {
                        let _ = e;
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(e) => anyhow::bail!("tls connect: {e}"),
                }
            };
            // Drain the server Hello.
            let _ = socket.next().await;
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec![topic_thread.clone()],
                },
            )
            .await?;
            ready_tx.send(()).ok();

            let mut out = Vec::new();
            let stop_at = Instant::now() + CLIENT_DEADLINE;
            while out.len() < 3 && Instant::now() < stop_at {
                match tokio::time::timeout(RECV_TIMEOUT, recv_envelope(&mut socket, codec)).await {
                    Ok(Ok(env)) if env.topic == topic_thread => out.push(env),
                    Ok(Ok(_)) => continue,
                    Ok(Err(_)) => break,
                    // Nothing yet — keep waiting until the outer deadline.
                    Err(_) => continue,
                }
            }
            Ok(out)
        })
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("tls client failed to subscribe");
    wait_for_subscribers(&server, &topic, 1);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(20))
        .count()
        .web_pub(&server, &topic)?;
    g.build().run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(500)),
    )?;

    let envs = client.join().expect("client thread panic")?;
    assert!(!envs.is_empty(), "no TLS envelopes received");
    for env in &envs {
        assert_eq!(env.topic, topic);
    }
    Ok(())
}
