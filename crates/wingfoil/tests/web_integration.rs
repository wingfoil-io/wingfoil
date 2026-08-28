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
    CONTROL_TOPIC, CodecKind, ControlMessage, Delivery, Envelope, WebBurstSinkOps, WebServer,
    WebSinkOps, web_sub,
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

/// The deadline for the `Delivery` loss tests, which move thousands of frames
/// rather than a handful. They are lossless by construction, so the client is
/// *supposed* to sit through the whole replay; a short deadline would truncate
/// the very thing under test. Still well under the workflow's job timeout.
///
/// It must comfortably exceed the **default `lossless_stall_timeout` (30 s)**,
/// which these probes run under: if the pipeline ever wedges, the server's
/// withdrawal fires at 30 s and the probe then reports a closed socket — a
/// diagnosable signature. This deadline used to be exactly 30 s, so the one
/// time a CI runner did wedge, the client deadline expired in the same breath
/// as the withdrawal and the failure said nothing about which happened.
const LOSS_PROBE_DEADLINE: Duration = Duration::from_secs(60);

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
    send_raw_payload(socket, codec, topic, payload).await
}

/// Send *already-serialized* payload bytes, wrapped in an envelope.
///
/// The browser never hands the codec a typed Rust value — `encodePayload`
/// produces the payload bytes itself (from `JSON.stringify`) and then frames
/// them. This is that shape, and it is what the browser-payload tests below
/// use so they exercise the real client → server byte path rather than a
/// Rust-to-Rust one.
async fn send_raw_payload(
    socket: &mut TungsteniteStream,
    codec: CodecKind,
    topic: &str,
    payload: Vec<u8>,
) -> anyhow::Result<()> {
    let env = Envelope {
        topic: topic.to_string(),
        time_ns: 0,
        payload,
    };
    let bytes = codec.encode(&env)?;
    socket.send(WsMessage::Binary(bytes.into())).await?;
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
/// connection's reader task. The fan-out drops anything published before the
/// connection is registered on the topic, so a test that starts its graph on
/// the `ready` signal alone can lose the whole sequence — which is exactly what
/// it looks like when `burst_stream_publishes_atomic_arrays` asserts against
/// `[]`.
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

/// **Browser → server under the JSON codec: the supported path.**
///
/// `sub_round_trip_bincode` above sends a *Rust* client's bytes — it encodes
/// a typed `UiClick` with bincode, which a browser can never do. This is the
/// browser's shape: the payload is raw JSON text, exactly what
/// `wingfoil-wasm`'s `encodePayload` produces from `JSON.stringify`, framed in
/// a JSON envelope. It is the only payload shape a browser can produce that
/// the server decodes correctly, which is why `@wingfoil/client` requires
/// `codec: "json"` for `publish`.
#[test]
fn sub_round_trip_json_browser_payload() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0")
        .codec(CodecKind::Json)
        .start()?;
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
                // What the browser sends: `JSON.stringify({button, count})`.
                let payload = format!(r#"{{"button":"ok","count":{i}}}"#).into_bytes();
                send_raw_payload(&mut socket, codec, "ui_events", payload).await?;
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

/// **Browser → server under bincode: silently corrupt, which is why the
/// browser client refuses to send it at all.**
///
/// The server half of `wingfoil-wasm`'s `bincode_payload_client_to_server_
/// would_corrupt`. A browser has no Rust schema, so the best payload it could
/// offer bincode is a schema-less `serde_json::Value` — encoded as a
/// length-prefixed map, while `web_sub`'s `codec.decode::<T>` runs
/// `deserialize_struct`, which reads bare fields in declaration order. The
/// mismatch is not reported: the server gets garbage. `encodePayload` now
/// errors instead of putting these bytes on the wire, so this pins *why*.
///
/// No socket: the corruption is in the codec, and `web_sub` decodes payload
/// bytes with exactly this call.
#[test]
fn browser_bincode_payload_does_not_decode_as_the_struct() {
    let click = UiClick {
        button: "ok".to_string(),
        count: 1,
    };
    let as_json = serde_json::to_value(&click).expect("UiClick is JSON-representable");
    let payload = CodecKind::Bincode
        .encode(&as_json)
        .expect("bincode encodes a Value as a map");

    match CodecKind::Bincode.decode::<UiClick>(&payload) {
        Ok(garbage) => assert_ne!(
            garbage, click,
            "a schema-less browser payload decoded correctly under bincode: if \
             this ever holds, revisit the encodePayload rejection in \
             crates/wingfoil-wasm/src/lib.rs"
        ),
        Err(_) => { /* also fine — either way the intended value never arrives */ }
    }
}

/// **Server → browser under bincode: undecodable, hence `decodePayload`
/// rejects it.**
///
/// `web_pub` writes the value with the server's codec. A browser decoding
/// that has no schema to decode *into*, and bincode carries no type tags, so
/// the only schema-less attempt available fails outright — which is what
/// `decodePayload` used to surface once per frame and now reports up front.
#[test]
fn server_bincode_payload_cannot_be_decoded_without_a_schema() {
    let payload = CodecKind::Bincode
        .encode(&UiClick {
            button: "ok".to_string(),
            count: 1,
        })
        .expect("bincode encodes the struct");

    let err = CodecKind::Bincode
        .decode::<serde_json::Value>(&payload)
        .expect_err("schema-less bincode decode must fail");
    assert!(
        format!("{err:#}").contains("deserialize_any"),
        "unexpected bincode error: {err:#}"
    );
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

    // 50 values, well under the outbound queue depth, so a loopback client
    // receives every one with no loss.
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

// ── Delivery policy (#437) ───────────────────────────────────────────────────
//
// A historical replay has no live clock to fall behind, so the real-time rule
// ("a frozen browser tab can never back-pressure the graph") corrupts it: a
// backtest running at CPU speed outruns any socket and the browser draws a
// replay with holes in it. `Delivery::Auto` resolves to lossless there and
// stays lossy in real time; these four tests pin both halves, plus the
// zero-client and multi-client policies.

/// How many frames the loss probes replay, and how big each one is.
///
/// Both numbers matter, and the size is the one that is easy to get wrong. The
/// lossy path buffers a frame several times over — the bounded sink channel
/// (64 instants), the 1024-slot per-connection outbound queue, and the kernel's
/// auto-tuned loopback socket buffer — and a stalled client drops nothing until
/// all of them are full. At a few dozen bytes a frame, thousands fit end to end
/// and a stalled client still receives every one, so the probe silently stops
/// probing. At ~2 KiB (a plausible snapshot to a browser) the same buffers hold
/// a few thousand frames, and a 12000-frame replay against a client that is not
/// reading overruns them by around a thousand — measured, on the pre-`Delivery`
/// path (which additionally ran a 1024-slot `broadcast`), at 11036 of these
/// 12000 delivered.
const LOSS_PROBE_FRAMES: u32 = 12000;
const LOSS_PROBE_PAYLOAD_WORDS: usize = 256;

/// Frame count for the two tests that assert a run is **not** paced. They only
/// have to outrun a wall-clock threshold, and their frames pile up in an
/// unbounded queue precisely because nothing is pacing them — so they stay
/// small.
const PACING_PROBE_FRAMES: u32 = 5000;

/// One probe frame: the sequence number in slot 0, filler after it.
fn probe_payload(seq: &u64) -> Vec<u64> {
    let mut payload = vec![0u64; LOSS_PROBE_PAYLOAD_WORDS];
    payload[0] = *seq;
    payload
}

/// How long the probe clients sit on the socket before reading a byte.
///
/// This is the other half of making the probe a real reproduction rather than a
/// hopeful one. A loopback client that only decodes keeps pace with a CPU-speed
/// replay frame for frame, so nothing is ever dropped and a lossy path looks
/// fine; the case #437 is about is a client that *stops reading for a while* —
/// a browser tab in the background, a stalled network. The probe clients below
/// therefore subscribe, then sit on the socket without reading for this long
/// before draining it flat out.
///
/// It has to be a fixed delay rather than a signal from the test: under a
/// lossless policy the graph is waiting for this client, so a client that
/// waited for the run to finish would deadlock the very configuration under
/// test.
const SLOW_CLIENT_STALL: Duration = Duration::from_millis(750);

/// Subscribe to `topic`, then collect its frames until the `Complete` marker
/// arrives. Returns the decoded values, whether the replay completed, and —
/// when it did not — how the read loop ended (`ended`), so a failure names the
/// mechanism (deadline vs a closed socket, i.e. a server-side withdrawal)
/// instead of erasing it.
///
/// `stall` holds the client off its first read for that long (see
/// [`SLOW_CLIENT_STALL`]); `None` reads from the start.
fn spawn_completion_subscriber(
    port: u16,
    topic: &'static str,
    codec: CodecKind,
    ready: std::sync::mpsc::Sender<()>,
    deadline: Duration,
    stall: Option<Duration>,
) -> std::thread::JoinHandle<anyhow::Result<(Vec<u64>, bool, String)>> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut socket = connect(port).await?;
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec![topic.to_string()],
                },
            )
            .await?;
            ready.send(()).ok();
            if let Some(stall) = stall {
                tokio::time::sleep(stall).await;
            }

            let mut values: Vec<u64> = Vec::new();
            let mut completed = false;
            let mut ended = String::new();
            let overall = Instant::now() + deadline;
            loop {
                if Instant::now() >= overall {
                    ended = format!("client deadline ({deadline:?}) expired");
                    break;
                }
                let env =
                    match tokio::time::timeout(RECV_TIMEOUT, recv_envelope(&mut socket, codec))
                        .await
                    {
                        Ok(Ok(env)) => env,
                        Ok(Err(e)) => {
                            ended = format!("socket ended: {e}");
                            break;
                        }
                        Err(_) => continue,
                    };
                if env.topic == topic {
                    values.push(codec.decode::<Vec<u64>>(&env.payload)?[0]);
                } else if env.topic == CONTROL_TOPIC
                    && let ControlMessage::Complete { topic: done } = codec.decode(&env.payload)?
                {
                    assert_eq!(done, topic);
                    completed = true;
                    break;
                }
            }
            Ok((values, completed, ended))
        })
    })
}

/// The regression this issue is about: a **fast** historical graph — one that
/// runs at CPU speed rather than being compute-bound — must deliver every frame
/// to a subscribed client, in order, and then complete.
///
/// Before `Delivery`, `web_pub` pushed into an unbounded channel drained into a
/// 1024-slot `broadcast` that cannot block its sender, and `forward_broadcast`
/// dropped on a full outbound queue. Neither can pace a replay, so the client
/// saw a corrupted subset. Under `Delivery::Auto` the historical run resolves
/// to lossless and the graph waits for the socket instead.
#[test]
fn historical_streaming_is_lossless_for_a_fast_graph() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    assert_eq!(server.delivery(), Delivery::Auto);
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let client = spawn_completion_subscriber(
        port,
        "fast",
        codec,
        ready_tx,
        LOSS_PROBE_DEADLINE,
        Some(SLOW_CLIENT_STALL),
    );
    ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "fast", 1);

    // No `Duration` pacing anywhere: under `HistoricalFrom` the ticker's
    // interval is logical time, so this replays as fast as the CPU allows.
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(probe_payload)
        .web_pub(&server, "fast")?;
    g.build().run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(LOSS_PROBE_FRAMES),
    )?;

    let (values, completed, ended) = client.join().expect("client thread panic")?;
    assert!(
        completed,
        "expected a Complete control frame at end of replay \
         (received {} of {LOSS_PROBE_FRAMES} frames; {ended})",
        values.len()
    );
    assert_eq!(
        values,
        (1..=u64::from(LOSS_PROBE_FRAMES)).collect::<Vec<u64>>(),
        "a fast historical replay must reach the client whole and in order"
    );
    Ok(())
}

/// Losslessness must not turn "nobody is watching" into a hang — the same class
/// of bug as the `web_sub` historical hang. With no subscriber there is nothing
/// to pace against, so the replay runs at full speed and simply drops its
/// frames on the floor — `deliver_to` returns immediately on an empty target
/// list.
#[test]
fn historical_lossless_does_not_hang_without_subscribers() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(probe_payload)
        .web_pub(&server, "nobody")?;

    let started = Instant::now();
    g.build().run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(PACING_PROBE_FRAMES),
    )?;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an unwatched historical replay must not pace itself against anything \
         (took {:?})",
        started.elapsed()
    );
    Ok(())
}

/// Multi-client policy: pace to the **slowest** subscriber, so every client
/// receives the whole replay rather than the fast one winning and the slow one
/// being truncated. The second client here deliberately reads on a delay.
#[test]
fn historical_lossless_paces_to_the_slowest_of_several_subscribers() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    // One client reads flat out, the other stalls; the replay must be paced to
    // the second without truncating either.
    let (fast_tx, fast_rx) = std::sync::mpsc::channel();
    let fast = spawn_completion_subscriber(port, "both", codec, fast_tx, LOSS_PROBE_DEADLINE, None);
    let (slow_tx, slow_rx) = std::sync::mpsc::channel();
    let slow = spawn_completion_subscriber(
        port,
        "both",
        codec,
        slow_tx,
        LOSS_PROBE_DEADLINE,
        Some(SLOW_CLIENT_STALL),
    );

    fast_rx.recv_timeout(Duration::from_secs(5))?;
    slow_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "both", 2);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(probe_payload)
        .web_pub(&server, "both")?;
    g.build().run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(LOSS_PROBE_FRAMES),
    )?;

    let expected = (1..=u64::from(LOSS_PROBE_FRAMES)).collect::<Vec<u64>>();
    let (fast_values, fast_done, fast_ended) = fast.join().expect("fast client panic")?;
    let (slow_values, slow_done, slow_ended) = slow.join().expect("slow client panic")?;
    assert!(
        fast_done,
        "fast client should see Complete \
         (received {} of {LOSS_PROBE_FRAMES} frames; {fast_ended})",
        fast_values.len()
    );
    assert!(
        slow_done,
        "slow client should see Complete \
         (received {} of {LOSS_PROBE_FRAMES} frames; {slow_ended})",
        slow_values.len()
    );
    assert_eq!(fast_values, expected, "fast subscriber lost frames");
    assert_eq!(slow_values, expected, "slow subscriber lost frames");
    Ok(())
}

/// The opt-out, and the guarantee real time still relies on: under
/// `Delivery::Lossy` **nothing a client does can stall the graph**, in either
/// run mode. This client subscribes and then never reads a byte, which would
/// park a lossless publisher on it forever; the replay must still finish.
#[test]
fn lossy_opt_out_never_lets_a_wedged_client_stall_the_graph() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0")
        .delivery(Delivery::Lossy)
        .start()?;
    assert_eq!(server.delivery(), Delivery::Lossy);
    let port = server.port();
    let codec = server.codec();

    // Subscribe, then go to sleep holding the socket open without reading.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let wedged = std::thread::spawn(move || -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        let mut socket = rt.block_on(connect(port))?;
        rt.block_on(send_control(
            &mut socket,
            codec,
            ControlMessage::Subscribe {
                topics: vec!["wedged".to_string()],
            },
        ))?;
        ready_tx.send(()).ok();
        // Hold the connection open (and unread) until the graph is done.
        release_rx.recv().ok();
        Ok(())
    });

    ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "wedged", 1);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(probe_payload)
        .web_pub(&server, "wedged")?;
    let started = Instant::now();
    g.build().run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(PACING_PROBE_FRAMES),
    )?;
    let elapsed = started.elapsed();

    release_tx.send(()).ok();
    wedged.join().expect("wedged client panic")?;
    assert!(
        elapsed < Duration::from_secs(5),
        "Delivery::Lossy must never back-pressure the graph (took {elapsed:?})"
    );
    Ok(())
}

/// Real time is unchanged: `Delivery::Auto` resolves to lossy there, so a
/// client that subscribes and never reads cannot hold up a live graph — the
/// rule the adapter has always run on.
#[test]
fn realtime_auto_stays_lossy_under_a_wedged_client() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let wedged = std::thread::spawn(move || -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        let mut socket = rt.block_on(connect(port))?;
        rt.block_on(send_control(
            &mut socket,
            codec,
            ControlMessage::Subscribe {
                topics: vec!["live".to_string()],
            },
        ))?;
        ready_tx.send(()).ok();
        release_rx.recv().ok();
        Ok(())
    });

    ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "live", 1);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_micros(100))
        .count()
        .web_pub(&server, "live")?;
    let started = Instant::now();
    g.build().run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(500)),
    )?;
    let elapsed = started.elapsed();

    release_tx.send(()).ok();
    wedged.join().expect("wedged client panic")?;
    assert!(
        elapsed < Duration::from_secs(3),
        "a realtime run must not pace itself against a client (took {elapsed:?})"
    );
    Ok(())
}

/// Lossless pacing must bound *death*, not just slowness. A half-open peer — a
/// dead machine, a partitioned network, a closed laptop lid — never closes its
/// socket, so "pace to the slowest subscriber" would otherwise mean "park until
/// TCP keepalive notices", hours later. Worse, the end-of-run `Complete` goes
/// out the same way, so `run()` itself would hang after the last cycle.
///
/// The client here subscribes and then never reads a byte, which is
/// indistinguishable from that. With `lossless_stall_timeout` the publisher
/// gives up on it and the replay finishes.
#[test]
fn lossless_withdraws_a_subscriber_that_stops_accepting_frames() -> anyhow::Result<()> {
    let stall = Duration::from_millis(300);
    let server = WebServer::bind("127.0.0.1:0")
        .delivery(Delivery::Lossless)
        .lossless_stall_timeout(stall)
        .start()?;
    assert_eq!(server.lossless_stall_timeout(), stall);
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let wedged = std::thread::spawn(move || -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        let mut socket = rt.block_on(connect(port))?;
        rt.block_on(send_control(
            &mut socket,
            codec,
            ControlMessage::Subscribe {
                topics: vec!["dead".to_string()],
            },
        ))?;
        ready_tx.send(()).ok();
        // Hold the socket open and unread — the half-open peer, simulated.
        release_rx.recv().ok();
        Ok(())
    });

    ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "dead", 1);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(probe_payload)
        .web_pub(&server, "dead")?;
    let started = Instant::now();
    g.build().run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(LOSS_PROBE_FRAMES),
    )?;
    let elapsed = started.elapsed();

    release_tx.send(()).ok();
    wedged.join().expect("wedged client panic")?;
    assert!(
        elapsed < Duration::from_secs(15),
        "a lossless run must not wait forever on a peer that has stopped \
         accepting frames (took {elapsed:?})"
    );
    Ok(())
}

/// Withdrawing a stalled subscriber has to *tell* it something, or the fix for
/// the unbounded wait strands the very clients it was meant to protect.
///
/// A registry-only withdrawal leaves the socket open and silent: no more data
/// frames, and no end-of-run `Complete` either, since `finally` snapshots a
/// registry the client has been removed from. `@wingfoil/client` keys both
/// `onComplete` and its stop-reconnecting logic on that marker, so such a
/// client neither completes nor retries — it just waits. The clients that reach
/// that state are the *recoverable* ones (a laptop that suspends, trips the
/// bound, then wakes); a genuinely dead peer never notices either way.
///
/// So the connection is closed, and this asserts the client can see it: it goes
/// quiet long enough to be declared gone, then finds its socket ended rather
/// than hanging on it.
#[test]
fn a_withdrawn_subscriber_gets_its_connection_closed() -> anyhow::Result<()> {
    let server = WebServer::bind("127.0.0.1:0")
        .delivery(Delivery::Lossless)
        .lossless_stall_timeout(Duration::from_millis(200))
        .start()?;
    let port = server.port();
    let codec = server.codec();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    // Releases the client from its quiet window. The client must stay quiet
    // until the server has actually given up on it — a fixed sleep instead
    // makes the test a race between the publisher filling a 1024-slot queue and
    // a wall clock, which a loaded machine loses: the client starts draining
    // early, the publisher never blocks for the stall bound, and nothing is
    // ever withdrawn. Waiting on the withdrawal itself has no such window.
    let (read_tx, read_rx) = std::sync::mpsc::channel::<()>();
    let client = std::thread::spawn(move || -> anyhow::Result<bool> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            let mut socket = connect(port).await?;
            send_control(
                &mut socket,
                codec,
                ControlMessage::Subscribe {
                    topics: vec!["withdrawn".to_string()],
                },
            )
            .await?;
            ready_tx.send(()).ok();

            // Go quiet until the server has declared us gone. Blocking the
            // client's runtime *is* the frozen-tab behaviour under test —
            // nothing polls the socket, so nothing drains it.
            read_rx
                .recv_timeout(CLIENT_DEADLINE)
                .map_err(|_| anyhow::anyhow!("never observed the withdrawal"))?;

            // A withdrawn client should find the stream ended, not hang on it.
            let deadline = Instant::now() + CLIENT_DEADLINE;
            while Instant::now() < deadline {
                match tokio::time::timeout(RECV_TIMEOUT, socket.next()).await {
                    // `None` = stream ended, `Err` = reset: both are the close.
                    Ok(None) | Ok(Some(Err(_))) => return Ok(true),
                    Ok(Some(Ok(_))) => continue,
                    Err(_) => continue,
                }
            }
            Ok(false)
        })
    });

    ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "withdrawn", 1);

    // The graph runs on its own thread so this one can watch for the
    // withdrawal while the replay is still in flight.
    let graph_server = &server;
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let graph = scope.spawn(move || -> anyhow::Result<()> {
            let g = GraphBuilder::new();
            let _sink = g
                .ticker(Duration::from_millis(1))
                .count()
                .map(probe_payload)
                .web_pub(graph_server, "withdrawn")?;
            g.build().run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(LOSS_PROBE_FRAMES),
            )?;
            Ok(())
        });

        // Withdrawal is observable as the subscriber leaving the registry.
        let deadline = Instant::now() + CLIENT_DEADLINE;
        while server.subscriber_count("withdrawn") > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            server.subscriber_count("withdrawn"),
            0,
            "a subscriber that accepts nothing for the stall bound must be withdrawn, \
             so subscriber_count stops reporting a client the publisher cannot reach"
        );
        read_tx.send(()).ok();

        graph.join().expect("graph thread panic")
    })?;

    let saw_close = client.join().expect("client thread panic")?;
    assert!(
        saw_close,
        "a subscriber the publisher gave up on must see its connection close, \
         so it can reconnect rather than wait on a socket that will never carry \
         another frame"
    );
    Ok(())
}

/// One `WebServer` can serve several graphs, and the resolved policy is
/// therefore **per publish**, not per server. Two graphs running at once in
/// opposite run modes must each get their own answer: a server-wide cell would
/// let whichever started second overwrite the first — flipping a live graph
/// into pacing on browser sockets, or reverting an in-flight replay to lossy.
///
/// The replay's client reads; the live graph's client is wedged. Both
/// assertions have to hold at once, and each fails under a shared cell
/// depending on which run resolved last.
#[test]
fn concurrent_runs_on_one_server_resolve_delivery_independently() -> anyhow::Result<()> {
    const LIVE_RUN: Duration = Duration::from_millis(500);

    let server = WebServer::bind("127.0.0.1:0").start()?;
    let port = server.port();
    let codec = server.codec();

    // The replay's subscriber: stalls, then drains — it must still get all of it.
    let (hist_ready_tx, hist_ready_rx) = std::sync::mpsc::channel();
    let hist_client = spawn_completion_subscriber(
        port,
        "concurrent_hist",
        codec,
        hist_ready_tx,
        LOSS_PROBE_DEADLINE,
        Some(SLOW_CLIENT_STALL),
    );

    // The live graph's subscriber: never reads. It must not pace anything.
    let (live_ready_tx, live_ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let wedged = std::thread::spawn(move || -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        let mut socket = rt.block_on(connect(port))?;
        rt.block_on(send_control(
            &mut socket,
            codec,
            ControlMessage::Subscribe {
                topics: vec!["concurrent_live".to_string()],
            },
        ))?;
        live_ready_tx.send(()).ok();
        release_rx.recv().ok();
        Ok(())
    });

    hist_ready_rx.recv_timeout(Duration::from_secs(5))?;
    live_ready_rx.recv_timeout(Duration::from_secs(5))?;
    wait_for_subscribers(&server, "concurrent_hist", 1);
    wait_for_subscribers(&server, "concurrent_live", 1);

    // The live graph runs on its own thread, overlapping the replay below.
    let live_server = &server;
    let live_elapsed = std::thread::scope(|scope| -> anyhow::Result<Duration> {
        let live = scope.spawn(move || -> anyhow::Result<Duration> {
            let g = GraphBuilder::new();
            let _sink = g
                .ticker(Duration::from_micros(100))
                .count()
                .map(probe_payload)
                .web_pub(live_server, "concurrent_live")?;
            let started = Instant::now();
            g.build()
                .run(RunMode::RealTime, RunFor::Duration(LIVE_RUN))?;
            Ok(started.elapsed())
        });

        let g = GraphBuilder::new();
        let _sink = g
            .ticker(Duration::from_millis(1))
            .count()
            .map(probe_payload)
            .web_pub(&server, "concurrent_hist")?;
        g.build().run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(LOSS_PROBE_FRAMES),
        )?;

        live.join().expect("live graph thread panic")
    })?;

    release_tx.send(()).ok();
    wedged.join().expect("wedged client panic")?;

    let (values, completed, ended) = hist_client.join().expect("client thread panic")?;
    assert!(
        completed,
        "the replay should still end with Complete \
         (received {} of {LOSS_PROBE_FRAMES} frames; {ended})",
        values.len()
    );
    assert_eq!(
        values,
        (1..=u64::from(LOSS_PROBE_FRAMES)).collect::<Vec<u64>>(),
        "the concurrent replay must stay lossless"
    );
    assert!(
        live_elapsed < LIVE_RUN + Duration::from_secs(2),
        "the concurrent live run must stay lossy — a wedged browser cannot be \
         allowed to pace it (took {live_elapsed:?})"
    );
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
            socket
                .send(WsMessage::Binary(vec![0xFFu8; 8].into()))
                .await?;
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
