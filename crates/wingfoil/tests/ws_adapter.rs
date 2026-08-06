//! Tests for the `ws` adapter.
//!
//! Every test runs against an in-process WebSocket server on loopback, so no
//! external service is needed and these are ordinary tier-1 tests:
//!
//! ```sh
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features ws --test ws_adapter
//! ```
//!
//! **These assert values, not tick times.** `ws_sub` is realtime-only by
//! construction (a live socket has no historical timeline), so its timestamps
//! are wall-clock reads and there is nothing deterministic to assert about
//! them. The historical-determinism convention applies to replay sources; the
//! contract here is *what arrives, and in what order*.
#![cfg(feature = "ws")]

use std::net::TcpListener as StdTcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use wingfoil::adapters::ws::{
    WsBackoff, WsConfig, WsMessage, WsSinkOps, WsStatus, ws_connect, ws_sub,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// Kept well under nextest's 10s `slow-timeout` (`.config/nextest.toml`), which
/// *terminates* a test rather than failing it — a run at or above that turns an
/// assertion failure into a kill with no message.
const RUN_FOR: Duration = Duration::from_millis(600);

/// What the loopback server does with each connection it accepts.
#[derive(Clone)]
enum Behaviour {
    /// Send these text frames, then hold the socket open.
    Send(Vec<String>),
    /// Send these text frames, then close — forcing the client to reconnect.
    SendThenClose(Vec<String>),
    /// Accept and say nothing, so only an idle timeout can notice.
    Silent,
    /// Send every frame received straight back.
    Echo,
    /// Accept the handshake and immediately close, without ever sending a
    /// frame — how a venue behaves when it rejects auth after the upgrade.
    CloseImmediately,
}

/// An in-process WebSocket server on an ephemeral loopback port.
struct TestServer {
    port: u16,
    /// Text frames the server received, across all connections.
    received: Arc<Mutex<Vec<String>>>,
    /// How many connections it has accepted.
    connections: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
}

impl TestServer {
    fn start(behaviour: Behaviour) -> anyhow::Result<Self> {
        // Bind synchronously so the port is live before any test wires a graph
        // against it — otherwise the client's first connect races the bind and
        // burns a reconnect attempt.
        let listener = StdTcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        let received = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let (thread_received, thread_connections, thread_shutdown) =
            (received.clone(), connections.clone(), shutdown.clone());

        std::thread::Builder::new()
            .name("ws-test-server".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("ws test server: building runtime: {e}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listener = match TcpListener::from_std(listener) {
                        Ok(l) => l,
                        Err(e) => {
                            eprintln!("ws test server: adopting listener: {e}");
                            return;
                        }
                    };
                    while !thread_shutdown.load(Ordering::Relaxed) {
                        let accept =
                            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                                .await;
                        let Ok(Ok((stream, _))) = accept else {
                            continue;
                        };
                        thread_connections.fetch_add(1, Ordering::Relaxed);
                        let behaviour = behaviour.clone();
                        let received = thread_received.clone();
                        tokio::spawn(async move {
                            serve(stream, behaviour, received).await;
                        });
                    }
                });
            })
            .map_err(|e| anyhow::anyhow!("spawning ws test server: {e}"))?;

        Ok(TestServer {
            port,
            received,
            connections,
            shutdown,
        })
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/stream", self.port)
    }

    fn received(&self) -> Vec<String> {
        self.received
            .lock()
            .expect("ws test server received mutex poisoned")
            .clone()
    }

    fn connection_count(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

async fn serve(
    stream: tokio::net::TcpStream,
    behaviour: Behaviour,
    received: Arc<Mutex<Vec<String>>>,
) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };

    match behaviour {
        Behaviour::Send(frames) => {
            for frame in frames {
                if socket.send(TungsteniteMessage::Text(frame)).await.is_err() {
                    return;
                }
            }
            // Hold the socket open, recording client frames.
            while let Some(Ok(msg)) = socket.next().await {
                record(&received, &msg);
            }
        }
        Behaviour::SendThenClose(frames) => {
            // Give the client a moment to finish sending its subscriptions, so
            // they are recorded before we tear the connection down.
            let deadline = Instant::now() + Duration::from_millis(60);
            while Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(20), socket.next()).await {
                    Ok(Some(Ok(msg))) => record(&received, &msg),
                    Ok(Some(Err(_)) | None) => return,
                    Err(_) => break,
                }
            }
            for frame in frames {
                if socket.send(TungsteniteMessage::Text(frame)).await.is_err() {
                    return;
                }
            }
            let _ = socket.close(None).await;
        }
        Behaviour::Silent => {
            while let Some(Ok(msg)) = socket.next().await {
                record(&received, &msg);
            }
        }
        Behaviour::CloseImmediately => {
            let _ = socket.close(None).await;
        }
        Behaviour::Echo => {
            while let Some(Ok(msg)) = socket.next().await {
                record(&received, &msg);
                if let TungsteniteMessage::Text(text) = msg
                    && socket.send(TungsteniteMessage::Text(text)).await.is_err()
                {
                    return;
                }
            }
        }
    }
}

fn record(received: &Arc<Mutex<Vec<String>>>, message: &TungsteniteMessage) {
    if let TungsteniteMessage::Text(text) = message {
        received
            .lock()
            .expect("ws test server received mutex poisoned")
            .push(text.clone());
    }
}

/// Backoff tuned for a test: fast, deterministic, and bounded.
fn quick_backoff(max_attempts: Option<u32>) -> WsBackoff {
    WsBackoff {
        initial: Duration::from_millis(20),
        max: Duration::from_millis(40),
        multiplier: 2.0,
        jitter: false,
        max_attempts,
    }
}

/// Collects everything a `Stream` ticks into a shared vec.
///
/// A realtime run cannot use `accumulate()` + `runner.value()` the way the
/// historical tests do, because the run ends on a wall-clock duration rather
/// than at a known cycle. The lock here is test-side, not on a production
/// graph path.
fn collector<T: Clone + Default + 'static>(stream: &Stream<T>) -> Arc<Mutex<Vec<T>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    stream.for_each(move |value: &T| {
        sink.lock()
            .expect("collector mutex poisoned")
            .push(value.clone());
        Ok(())
    });
    seen
}

/// The message of a wiring error, or a panic naming what should have failed.
///
/// `Stream` is deliberately not `Debug`, so `Result::expect_err` is unavailable
/// on a factory's return type.
fn wiring_error<T>(result: anyhow::Result<T>, expectation: &str) -> String {
    match result {
        Ok(_) => panic!("expected a wiring error: {expectation}"),
        Err(e) => format!("{e:#}"),
    }
}

fn texts(frames: &[Burst<WsMessage>]) -> Vec<String> {
    frames
        .iter()
        .flat_map(|burst| burst.iter())
        .filter_map(|m| m.as_text().map(str::to_owned))
        .collect()
}

// ---------------------------------------------------------------------------
// Wiring-time validation
// ---------------------------------------------------------------------------

#[test]
fn historical_run_mode_is_rejected_at_wiring() {
    let g = GraphBuilder::new();
    let message = wiring_error(
        ws_sub(
            &g,
            RunMode::HistoricalFrom(NanoTime::ZERO),
            "ws://127.0.0.1:1/stream",
        ),
        "a live socket has no historical timeline to replay",
    );
    assert!(
        message.contains("HistoricalFrom is unsupported"),
        "unexpected error: {message}"
    );
}

#[test]
fn a_non_websocket_url_is_rejected_at_wiring() {
    let g = GraphBuilder::new();
    let message = wiring_error(
        ws_sub(&g, RunMode::RealTime, "https://example.com/stream"),
        "https:// is not a WebSocket scheme",
    );
    assert!(
        message.contains("not a WebSocket URL"),
        "unexpected error: {message}"
    );
}

/// `wss://` without the TLS feature must fail loudly at wiring rather than at
/// the first connect attempt, where the backoff loop would bury it.
#[cfg(not(feature = "ws-tls"))]
#[test]
fn wss_without_the_tls_feature_is_rejected_at_wiring() {
    let g = GraphBuilder::new();
    let message = wiring_error(
        ws_sub(&g, RunMode::RealTime, "wss://example.com/stream"),
        "wss:// needs the ws-tls feature",
    );
    assert!(
        message.contains("ws-tls"),
        "the error should name the feature that fixes it: {message}"
    );
}

/// Credentials must not reach an error message — this is the assertion that
/// keeps `redacted()` wired up at every error site.
#[test]
fn wiring_errors_never_leak_credentials() {
    let g = GraphBuilder::new();
    let message = wiring_error(
        ws_sub(
            &g,
            RunMode::RealTime,
            "http://alice:hunter2@example.com/stream?api_key=abc123",
        ),
        "http:// is not a WebSocket scheme",
    );
    assert!(!message.contains("hunter2"), "leaked a password: {message}");
    assert!(!message.contains("abc123"), "leaked an api key: {message}");
}

// ---------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------

#[test]
fn frames_arrive_in_order() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::Send(vec![
        "first".into(),
        "second".into(),
        "third".into(),
    ]))?;

    let g = GraphBuilder::new();
    let frames = ws_sub(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url()).backoff(quick_backoff(None)),
    )?;
    let seen = collector(&frames);

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    let received = texts(&seen.lock().expect("collector mutex poisoned"));
    assert_eq!(received, vec!["first", "second", "third"]);
    Ok(())
}

#[test]
fn subscriptions_are_sent_on_connect() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::Send(vec!["tick".into()]))?;

    let g = GraphBuilder::new();
    let frames = ws_sub(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url())
            .subscribe(r#"{"op":"subscribe","args":["trades"]}"#)
            .subscribe(r#"{"op":"subscribe","args":["book"]}"#)
            .backoff(quick_backoff(None)),
    )?;
    let seen = collector(&frames);

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    // Sent in the configured order, which venues that key off subscription
    // sequence depend on.
    assert_eq!(
        server.received(),
        vec![
            r#"{"op":"subscribe","args":["trades"]}"#,
            r#"{"op":"subscribe","args":["book"]}"#
        ]
    );
    assert_eq!(
        texts(&seen.lock().expect("collector mutex poisoned")),
        vec!["tick"]
    );
    Ok(())
}

/// The reason this adapter exists: a venue that drops you leaves a *live*
/// socket with no subscriptions on it unless something re-sends them.
#[test]
fn subscriptions_are_resent_after_a_reconnect() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::SendThenClose(vec!["hello".into()]))?;

    let g = GraphBuilder::new();
    let frames = ws_sub(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url())
            .subscribe("SUBSCRIBE")
            .backoff(quick_backoff(None)),
    )?;
    let seen = collector(&frames);

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    assert!(
        server.connection_count() >= 2,
        "expected a reconnect, saw {} connection(s)",
        server.connection_count()
    );
    let subscribes = server.received();
    assert!(
        subscribes.len() >= 2,
        "every connection must re-send the subscription, got {subscribes:?}"
    );
    assert!(
        subscribes.iter().all(|s| s == "SUBSCRIBE"),
        "unexpected client frames: {subscribes:?}"
    );
    // And the feed keeps producing across the reconnect.
    let received = texts(&seen.lock().expect("collector mutex poisoned"));
    assert!(
        received.len() >= 2,
        "expected frames from more than one connection, got {received:?}"
    );
    Ok(())
}

/// A venue that stops sending without closing the TCP connection is invisible
/// at the socket level; only the idle timeout catches it.
#[test]
fn an_idle_connection_is_dropped_and_retried() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::Silent)?;

    let g = GraphBuilder::new();
    // Nothing consumes the frames here; the nodes are already wired into the
    // builder, and this test's contract is the reconnect, not the payload.
    let _frames = ws_sub(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url())
            .idle_timeout(Duration::from_millis(60))
            .backoff(quick_backoff(None)),
    )?;

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    assert!(
        server.connection_count() >= 2,
        "the idle timeout should have forced a reconnect, saw {} connection(s)",
        server.connection_count()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[test]
fn status_reports_connected_then_reconnecting() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::SendThenClose(vec!["one".into()]))?;

    let g = GraphBuilder::new();
    let connection = ws_connect(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url()).backoff(quick_backoff(None)),
    )?;
    let statuses = collector(&connection.status);

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    let seen = statuses.lock().expect("collector mutex poisoned").clone();
    assert_eq!(
        seen.first(),
        Some(&WsStatus::Connected),
        "the first transition should be Connected, got {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|s| matches!(s, WsStatus::Reconnecting { .. })),
        "a dropped connection should report Reconnecting, got {seen:?}"
    );
    // Transitions only — never the same state twice in a row.
    assert!(
        seen.windows(2).all(|w| w[0] != w[1]),
        "status must tick on transitions only, got {seen:?}"
    );
    Ok(())
}

/// The one way this source ever ends: the backoff gives up and the run aborts
/// with a message naming the endpoint.
#[test]
fn exhausted_backoff_aborts_the_run() -> anyhow::Result<()> {
    // Bind and drop, so the port is real but certain to refuse connections.
    let dead_port = {
        let listener = StdTcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    };

    let g = GraphBuilder::new();
    let _frames = ws_sub(
        &g,
        RunMode::RealTime,
        WsConfig::new(format!("ws://127.0.0.1:{dead_port}/stream")).backoff(quick_backoff(Some(2))),
    )?;

    let err = g
        .build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))
        .expect_err("the run should abort once the backoff is exhausted");
    let message = format!("{err:#}");
    assert!(
        message.contains("giving up on") && message.contains("after 2"),
        "unexpected error: {message}"
    );
    // The *reason* must survive. Without it the message says only that N
    // attempts failed, which cannot distinguish a refused port from a DNS
    // failure or a TLS handshake error — the one thing it is most needed for.
    assert!(
        message.contains("last error:"),
        "the give-up message must carry the last connect error: {message}"
    );
    Ok(())
}

/// A handshake that completes and then dies must still count against
/// `max_attempts`.
///
/// Resetting the retry counter when `connect_async` returns `Ok` makes the
/// limit unreachable here: the connect keeps succeeding, so the count never
/// climbs, and the loop reconnects at `initial` forever against a venue that
/// has already told us no. Rejected auth, a malformed subscription and an IP
/// ban all present exactly like this, so it is the case the limit is most
/// needed for — and the one a refused-port test cannot reach.
#[test]
fn a_flapping_connection_still_exhausts_the_backoff() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::CloseImmediately)?;

    let g = GraphBuilder::new();
    let _frames = ws_sub(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url()).backoff(quick_backoff(Some(2))),
    )?;

    let err = g
        .build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))
        .expect_err("a connection that closes on contact must exhaust max_attempts");
    let message = format!("{err:#}");
    assert!(
        message.contains("giving up on") && message.contains("after 2"),
        "unexpected error: {message}"
    );
    // Bounded by max_attempts rather than by how many round trips fit in
    // RUN_FOR — the pre-fix loop managed 27.
    let connections = server.connections.load(Ordering::SeqCst);
    assert!(
        connections <= 4,
        "expected the backoff to stop after ~3 connections, got {connections}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

#[test]
fn ws_send_delivers_graph_values_to_the_venue() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::Echo)?;

    let g = GraphBuilder::new();
    let connection = ws_connect(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url()).backoff(quick_backoff(None)),
    )?;
    let echoed = collector(&connection.messages);

    // One outbound frame per tick, echoed straight back by the server.
    g.ticker(Duration::from_millis(50))
        .map(|_| WsMessage::from("ping-from-graph"))
        .ws_send(&connection.sender);

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    let sent = server.received();
    assert!(
        sent.iter().any(|s| s == "ping-from-graph"),
        "the server never saw the graph's frame, got {sent:?}"
    );
    let back = texts(&echoed.lock().expect("collector mutex poisoned"));
    assert!(
        back.iter().any(|s| s == "ping-from-graph"),
        "the echo never reached the graph, got {back:?}"
    );
    Ok(())
}

/// Frames queued before the socket is up are held, not dropped — the sender
/// exists from wiring but the connection only at run start.
#[test]
fn frames_sent_before_connect_are_queued() -> anyhow::Result<()> {
    let server = TestServer::start(Behaviour::Echo)?;

    let g = GraphBuilder::new();
    let connection = ws_connect(
        &g,
        RunMode::RealTime,
        WsConfig::new(server.url()).backoff(quick_backoff(None)),
    )?;

    // Before the graph has run at all, so before any socket exists.
    connection.sender.send("queued-early")?;

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(RUN_FOR))?;

    assert!(
        server.received().iter().any(|s| s == "queued-early"),
        "a frame queued before connect should still be delivered, got {:?}",
        server.received()
    );
    Ok(())
}
