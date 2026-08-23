//! HTTP + WebSocket server used by the `web` adapter.
//!
//! [`WebServer`] binds a TCP port synchronously (so bind errors surface
//! before the graph starts) and spawns an axum server on its own dedicated
//! tokio runtime — the same pattern used by the Prometheus exporter.
//! Graph nodes (`web_pub`, `web_sub`) register topics with the server at
//! construction time; the server stays alive for the lifetime of the
//! [`WebServer`] handle (or until [`WebServer::stop`] is called).

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Context as _;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tower_http::services::ServeDir;
use wingfoil::RunMode;

use super::codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION};

/// Per-topic broadcast capacity for server → client publishes. Slow
/// consumers that cannot drain at this rate receive
/// [`broadcast::error::RecvError::Lagged`] instead of blocking the graph.
pub(crate) const PUBLISH_BROADCAST_CAPACITY: usize = 1024;

/// Per-connection WS outbound queue depth. Bounded so a slow socket
/// cannot grow memory without bound.
pub(crate) const CONNECTION_OUTBOUND_CAPACITY: usize = 1024;

/// Per-subscribed-topic mpsc capacity (client → graph). Bounded so a
/// misbehaving client cannot grow memory without bound.
pub(crate) const SUBSCRIBE_MPSC_CAPACITY: usize = 1024;

/// How server → client publishes behave when a client cannot keep up.
///
/// Real-time and historical want opposite things, which is why this is a knob
/// rather than a constant:
///
/// * **Real time** has a live clock. A client that falls behind is *already*
///   showing stale data, and the only alternative to dropping frames is to
///   stall the graph — so a frozen browser tab would back-pressure a trading
///   system. Dropping is right.
/// * **Historical replay** has no live clock to fall behind. A backtest running
///   at CPU speed outruns any socket, so dropping frames does not "keep up with
///   real time" — it just corrupts the replay the browser is drawing.
///
/// [`Delivery::Auto`] (the default) picks per run mode, so the sensible
/// behaviour is the default and neither mode needs the caller to think about
/// it. See [`WebServerBuilder::delivery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Lossy under [`RunMode::RealTime`], lossless under
    /// [`RunMode::HistoricalFrom`]. The default, and what almost every graph
    /// wants.
    #[default]
    Auto,
    /// Always lossy: a client that cannot keep up drops frames, and nothing a
    /// client does can ever stall the graph. This is the pre-`Delivery`
    /// behaviour in both run modes.
    Lossy,
    /// Always lossless: the publisher paces itself to the slowest *subscribed*
    /// client, so every frame reaches every subscriber in order.
    ///
    /// With **no** subscribers the publisher never waits — frames are simply
    /// dropped on the floor as before, so a server-less or unwatched run does
    /// not hang. A client that stops reading without closing its socket *will*
    /// stall the graph; that is the contract, and it is why this is not the
    /// real-time default.
    Lossless,
}

/// Resolved delivery, as stored in [`WebServerInner::resolved`]. `UNRESOLVED`
/// is the pre-run state — no graph has started, so nothing is publishing yet;
/// it reads as lossy so a stray publish behaves exactly as it used to.
const RESOLVED_UNRESOLVED: u8 = 0;
const RESOLVED_LOSSY: u8 = 1;
const RESOLVED_LOSSLESS: u8 = 2;

/// One client's subscription to one publish topic, on the lossless path: the
/// connection's own outbound queue, plus an id so the connection can withdraw
/// it on `Unsubscribe` or close.
pub(crate) struct LosslessSub {
    id: u64,
    tx: mpsc::Sender<Bytes>,
}

pub(crate) struct WebServerInner {
    pub(crate) codec: CodecKind,
    /// The configured delivery policy. [`Delivery::Auto`] is turned into a
    /// concrete one at graph start — see [`WebServerInner::resolve_delivery`].
    pub(crate) delivery: Delivery,
    /// The delivery policy in force for the current run: one of the
    /// `RESOLVED_*` constants. Written once per run (at graph `start`), then
    /// read per instant on the graph thread and per instant on the publish
    /// consumer task — an atomic rather than a lock because the first of those
    /// is the graph execution path.
    resolved: AtomicU8,
    /// Topics the graph publishes. Each WS connection that subscribes to
    /// `topic` gets its own broadcast receiver. Frames flow as
    /// refcounted [`Bytes`] so they can be forwarded without copying.
    ///
    /// This is the **lossy** fan-out. It is also what
    /// [`WebServer::subscriber_count`] counts, in both modes: a connection
    /// registers here and on [`Self::lossless_subs`] in the same step, so the
    /// count means the same thing either way.
    pub(crate) pub_topics: Mutex<HashMap<String, broadcast::Sender<Bytes>>>,
    /// The **lossless** fan-out: per topic, one entry per subscribed
    /// connection, holding that connection's bounded outbound queue directly.
    /// The publisher awaits each `send`, so a slow socket paces the graph
    /// instead of losing frames. Empty ⇒ nothing to wait for.
    pub(crate) lossless_subs: Mutex<HashMap<String, Vec<LosslessSub>>>,
    /// Hands out [`LosslessSub::id`]s — unique per server, so a connection can
    /// withdraw exactly its own subscription.
    next_sub_id: AtomicU64,
    /// Topics the graph consumes from the browser. When a client frame
    /// arrives on one of these topics we forward the raw payload bytes
    /// to every registered mpsc sender. There is usually one sender per
    /// `web_sub::<T>()` call.
    pub(crate) sub_topics: Mutex<HashMap<String, Vec<mpsc::Sender<Bytes>>>>,
}

impl WebServerInner {
    fn new(codec: CodecKind, delivery: Delivery) -> Self {
        Self {
            codec,
            delivery,
            resolved: AtomicU8::new(RESOLVED_UNRESOLVED),
            pub_topics: Mutex::new(HashMap::new()),
            lossless_subs: Mutex::new(HashMap::new()),
            next_sub_id: AtomicU64::new(0),
            sub_topics: Mutex::new(HashMap::new()),
        }
    }

    /// Turn the configured [`Delivery`] into a concrete one for this run.
    ///
    /// Called from `web_pub`'s graph `start` hook, which is the earliest point
    /// the run mode exists — the server is built before the graph runs, and a
    /// `WebServer` can serve more than one run, so this is a per-run decision
    /// rather than a construction-time one. It runs before the first cycle, so
    /// no frame is ever published against an unresolved policy.
    pub(crate) fn resolve_delivery(&self, run_mode: RunMode) {
        let resolved = match (self.delivery, run_mode) {
            (Delivery::Lossy, _) => RESOLVED_LOSSY,
            (Delivery::Lossless, _) => RESOLVED_LOSSLESS,
            (Delivery::Auto, RunMode::RealTime) => RESOLVED_LOSSY,
            (Delivery::Auto, RunMode::HistoricalFrom(_)) => RESOLVED_LOSSLESS,
        };
        self.resolved.store(resolved, Ordering::Release);
    }

    /// Whether the current run delivers losslessly. Read per frame on the
    /// graph thread and on the consumer task, so it is a plain atomic load —
    /// no lock touches the execution path.
    pub(crate) fn is_lossless(&self) -> bool {
        self.resolved.load(Ordering::Acquire) == RESOLVED_LOSSLESS
    }

    /// Register a connection's outbound queue on the lossless path for
    /// `topic`, returning the id needed to withdraw it again.
    fn register_lossless_sub(&self, topic: &str, tx: mpsc::Sender<Bytes>) -> u64 {
        let id = self.next_sub_id.fetch_add(1, Ordering::Relaxed);
        self.lossless_subs
            .lock()
            .expect("lossless_subs lock poisoned")
            .entry(topic.to_string())
            .or_default()
            .push(LosslessSub { id, tx });
        id
    }

    /// Withdraw one connection's lossless subscription to `topic`.
    fn remove_lossless_sub(&self, topic: &str, id: u64) {
        let mut guard = self
            .lossless_subs
            .lock()
            .expect("lossless_subs lock poisoned");
        if let Some(subs) = guard.get_mut(topic) {
            subs.retain(|s| s.id != id);
            if subs.is_empty() {
                guard.remove(topic);
            }
        }
    }

    /// Deliver `bytes` to every client currently subscribed to `topic`,
    /// **awaiting** each one — so the caller (the publish consumer task, which
    /// the graph is paced against) waits for the slowest subscriber rather than
    /// dropping the frame.
    ///
    /// Zero subscribers is not an error and never waits: the frame is dropped,
    /// exactly as a `broadcast::send` with no receivers is. A subscriber whose
    /// queue has closed (its socket died) is withdrawn rather than waited on.
    ///
    /// The senders are snapshotted out of the mutex before any `await`, so no
    /// lock is ever held across a suspension point.
    pub(crate) async fn deliver_lossless(&self, topic: &str, bytes: Bytes) {
        let snapshot: Vec<(u64, mpsc::Sender<Bytes>)> = {
            let guard = self
                .lossless_subs
                .lock()
                .expect("lossless_subs lock poisoned");
            match guard.get(topic) {
                Some(subs) => subs.iter().map(|s| (s.id, s.tx.clone())).collect(),
                None => return,
            }
        };
        for (id, tx) in snapshot {
            if tx.send(bytes.clone()).await.is_err() {
                // The connection is gone; stop holding the graph up for it.
                self.remove_lossless_sub(topic, id);
            }
        }
    }

    pub(crate) fn get_or_create_pub_topic(&self, topic: &str) -> broadcast::Sender<Bytes> {
        let mut guard = self.pub_topics.lock().expect("pub_topics lock poisoned");
        guard
            .entry(topic.to_string())
            .or_insert_with(|| broadcast::channel(PUBLISH_BROADCAST_CAPACITY).0)
            .clone()
    }

    pub(crate) fn register_sub_sender(&self, topic: &str, tx: mpsc::Sender<Bytes>) {
        let mut guard = self.sub_topics.lock().expect("sub_topics lock poisoned");
        guard.entry(topic.to_string()).or_default().push(tx);
    }

    /// Forward a payload received from a WS client to every registered
    /// sub listener on this topic. Drops listeners whose mpsc is closed.
    fn dispatch_client_payload(&self, topic: &str, payload: Bytes) {
        let mut guard = self.sub_topics.lock().expect("sub_topics lock poisoned");
        if let Some(senders) = guard.get_mut(topic) {
            senders.retain(|tx| match tx.try_send(payload.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::warn!("web_sub: topic '{topic}' listener overloaded — dropping frame");
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
        }
    }
}

/// Optional TLS material for the [`WebServer`]. Loaded once on
/// [`WebServerBuilder::start`] from PEM files on disk.
#[cfg(feature = "web-tls")]
struct TlsPaths {
    cert_path: PathBuf,
    key_path: PathBuf,
}

/// Handle to a running HTTP + WebSocket server.
///
/// Dropping or calling [`WebServer::stop`] shuts down the axum server and
/// joins the server thread.
pub struct WebServer {
    pub(crate) inner: Arc<WebServerInner>,
    port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    historical_noop: bool,
    tls: bool,
}

impl WebServer {
    /// Start configuring a new server that will bind to `addr`.
    ///
    /// Use `"127.0.0.1:0"` to let the OS assign a port, then read it
    /// back with [`WebServer::port`] after [`WebServerBuilder::start`].
    pub fn bind(addr: impl Into<String>) -> WebServerBuilder {
        WebServerBuilder {
            addr: addr.into(),
            codec: CodecKind::Bincode,
            delivery: Delivery::default(),
            static_dir: None,
            #[cfg(feature = "web-tls")]
            tls: None,
        }
    }

    /// The port the server bound on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The codec the server is using.
    pub fn codec(&self) -> CodecKind {
        self.inner.codec
    }

    /// The [`Delivery`] policy the server was built with.
    ///
    /// This is the *configured* value: [`Delivery::Auto`] stays `Auto` here,
    /// because which of lossy/lossless it means is a property of the run, not
    /// of the server — it is decided at each graph's `start`.
    pub fn delivery(&self) -> Delivery {
        self.inner.delivery
    }

    /// True when the server was created as a historical-mode no-op.
    pub fn is_historical_noop(&self) -> bool {
        self.historical_noop
    }

    /// How many connected clients a publish on `topic` would currently reach.
    ///
    /// A client's `Subscribe` is processed asynchronously by its connection's
    /// reader task, so a client that has *sent* one is not yet receiving:
    /// [`tokio::sync::broadcast`] delivers only to receivers that already
    /// exist, and a frame published before then is dropped, not queued. This
    /// counts the receivers the publisher can actually reach, so it steps from
    /// 0 to 1 exactly when a publish would start being delivered — which makes
    /// it the thing to wait on before publishing a short, finite sequence.
    pub fn subscriber_count(&self, topic: &str) -> usize {
        self.inner
            .pub_topics
            .lock()
            .expect("pub_topics lock poisoned")
            .get(topic)
            .map_or(0, |sender| sender.receiver_count())
    }

    /// True when the server is terminating TLS (i.e. clients should
    /// connect via `https://` / `wss://`).
    pub fn is_tls(&self) -> bool {
        self.tls
    }

    /// Stop the HTTP server and join the server thread. Called
    /// automatically on drop.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Builder for [`WebServer`].
pub struct WebServerBuilder {
    addr: String,
    codec: CodecKind,
    delivery: Delivery,
    static_dir: Option<PathBuf>,
    #[cfg(feature = "web-tls")]
    tls: Option<TlsPaths>,
}

impl WebServerBuilder {
    /// Set the wire codec (default: [`CodecKind::Bincode`]).
    pub fn codec(mut self, codec: CodecKind) -> Self {
        self.codec = codec;
        self
    }

    /// Choose how publishes behave when a client cannot keep up
    /// (default: [`Delivery::Auto`] — lossy in real time, lossless in a
    /// historical replay).
    ///
    /// ```ignore
    /// // Never let a client pace the graph, even in a backtest.
    /// let server = WebServer::bind("127.0.0.1:0")
    ///     .delivery(Delivery::Lossy)
    ///     .start()?;
    /// ```
    pub fn delivery(mut self, delivery: Delivery) -> Self {
        self.delivery = delivery;
        self
    }

    /// Serve static files from `dir` under `GET /` alongside the
    /// WebSocket endpoint. Useful for hosting the `wingfoil-js` UI
    /// bundle from the same origin.
    pub fn serve_static(mut self, dir: impl Into<PathBuf>) -> Self {
        self.static_dir = Some(dir.into());
        self
    }

    /// Terminate TLS using the PEM-encoded certificate chain at
    /// `cert_path` and the private key at `key_path`. Clients must
    /// connect via `https://` / `wss://`.
    ///
    /// Available behind the `web-tls` cargo feature. Files are read at
    /// [`WebServerBuilder::start`] time; an unreadable or malformed
    /// cert/key surfaces synchronously as an error before the graph
    /// starts (same property as the bind step). The active rustls
    /// crypto provider is `ring`, matching the FIX adapter — install
    /// it ahead of time via `rustls::crypto::CryptoProvider` only if
    /// you're sharing a process-wide default with other crates.
    ///
    /// ```ignore
    /// let server = WebServer::bind("0.0.0.0:8080")
    ///     .serve_static("./dist")
    ///     .tls("/etc/wingfoil/tls/cert.pem", "/etc/wingfoil/tls/key.pem")
    ///     .start()?;
    /// ```
    #[cfg(feature = "web-tls")]
    pub fn tls(mut self, cert_path: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        self.tls = Some(TlsPaths {
            cert_path: cert_path.into(),
            key_path: key_path.into(),
        });
        self
    }

    /// Bind the TCP listener and spawn the HTTP + WS server.
    ///
    /// Binding is synchronous so a port conflict is reported
    /// immediately, before the graph starts. When TLS is configured
    /// via [`WebServerBuilder::tls`] the cert and key are loaded
    /// synchronously here too — so a missing or malformed PEM also
    /// surfaces before the graph starts.
    pub fn start(self) -> anyhow::Result<WebServer> {
        let listener =
            TcpListener::bind(&self.addr).with_context(|| format!("web: bind to {}", self.addr))?;
        let port = listener.local_addr().context("web: local_addr")?.port();

        // Load TLS material before spawning the server thread so any
        // error (file missing, bad PEM, no key/cert pair) surfaces here
        // alongside bind errors instead of inside the spawned task.
        #[cfg(feature = "web-tls")]
        let tls_config = match self.tls {
            Some(paths) => Some(load_tls_config(&paths)?),
            None => None,
        };
        #[cfg(feature = "web-tls")]
        let is_tls = tls_config.is_some();
        #[cfg(not(feature = "web-tls"))]
        let is_tls = false;

        let inner = Arc::new(WebServerInner::new(self.codec, self.delivery));
        let inner_clone = inner.clone();
        let static_dir = self.static_dir.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let handle = std::thread::Builder::new()
            .name("wingfoil-web".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        log::error!("web: failed to build runtime: {e}");
                        return;
                    }
                };
                rt.block_on(async move {
                    let app = build_router(inner_clone, static_dir);

                    #[cfg(feature = "web-tls")]
                    if let Some(cfg) = tls_config {
                        serve_tls(listener, app, cfg, shutdown_rx).await;
                        return;
                    }

                    listener
                        .set_nonblocking(true)
                        .expect("listener set_nonblocking");
                    let tokio_listener =
                        tokio::net::TcpListener::from_std(listener).expect("web: from_std");
                    if let Err(e) = axum::serve(tokio_listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await
                    {
                        log::warn!("web: axum serve exited with error: {e}");
                    }
                });
            })
            .context("web: spawn server thread")?;

        Ok(WebServer {
            inner,
            port,
            shutdown_tx: Some(shutdown_tx),
            thread: Some(handle),
            historical_noop: false,
            tls: is_tls,
        })
    }

    /// Build a no-op server for historical-mode runs. No TCP port is
    /// bound; all publishes and subscribes become no-ops.
    pub fn start_historical(self) -> anyhow::Result<WebServer> {
        Ok(WebServer {
            inner: Arc::new(WebServerInner::new(self.codec, self.delivery)),
            port: 0,
            shutdown_tx: None,
            thread: None,
            historical_noop: true,
            tls: false,
        })
    }
}

fn build_router(inner: Arc<WebServerInner>, static_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(inner);
    if let Some(dir) = static_dir {
        router = router.fallback_service(ServeDir::new(dir));
    }
    router
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(inner): State<Arc<WebServerInner>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, inner))
}

/// Per-connection task. Outbound (server → client) uses one mpsc queue
/// drained by a writer task; one forwarder task per subscribed pub topic
/// pushes frames into it. Inbound (client → server) is handled inline in
/// the reader loop below.
async fn handle_socket(socket: WebSocket, inner: Arc<WebServerInner>) {
    let codec = inner.codec;
    let (mut ws_sink, mut ws_stream) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Bytes>(CONNECTION_OUTBOUND_CAPACITY);

    let writer = tokio::spawn(async move {
        while let Some(bytes) = outbound_rx.recv().await {
            if ws_sink.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
        let _ = ws_sink.close().await;
    });

    let hello = ControlMessage::Hello {
        codec,
        version: WIRE_PROTOCOL_VERSION,
    };
    let hello_bytes = match encode_control_frame(codec, &hello) {
        Ok(b) => b,
        Err(e) => {
            log::error!("web: encode hello failed: {e}");
            writer.abort();
            return;
        }
    };
    if outbound_tx.send(hello_bytes).await.is_err() {
        return;
    }

    let mut forwarders: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    // The lossless-path counterpart of `forwarders`: one id per subscribed
    // topic, so this connection can withdraw exactly its own registration.
    // A subscription always registers on *both* paths, so `subscriber_count`
    // means the same thing whichever policy the run resolves to; the publisher
    // then uses one path or the other, never both, so nothing is duplicated.
    let mut lossless_ids: HashMap<String, u64> = HashMap::new();

    while let Some(msg) = ws_stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                log::debug!("web: ws recv error: {e}");
                break;
            }
        };
        let bytes: Bytes = match msg {
            Message::Binary(b) => b,
            Message::Text(t) => Bytes::copy_from_slice(t.as_bytes()),
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        let env: Envelope = match codec.decode(&bytes) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("web: bad envelope from client: {e}");
                continue;
            }
        };
        if env.topic == CONTROL_TOPIC {
            let ctrl: ControlMessage = match codec.decode(&env.payload) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("web: bad control payload: {e}");
                    continue;
                }
            };
            match ctrl {
                ControlMessage::Subscribe { topics } => {
                    for topic in topics {
                        if forwarders.contains_key(&topic) {
                            continue;
                        }
                        let sender = inner.get_or_create_pub_topic(&topic);
                        let rx = sender.subscribe();
                        let id = inner.register_lossless_sub(&topic, outbound_tx.clone());
                        lossless_ids.insert(topic.clone(), id);
                        let out = outbound_tx.clone();
                        let topic_for_log = topic.clone();
                        let handle = tokio::spawn(async move {
                            forward_broadcast(topic_for_log, rx, out).await;
                        });
                        forwarders.insert(topic, handle);
                    }
                }
                ControlMessage::Unsubscribe { topics } => {
                    for topic in topics {
                        if let Some(h) = forwarders.remove(&topic) {
                            h.abort();
                        }
                        if let Some(id) = lossless_ids.remove(&topic) {
                            inner.remove_lossless_sub(&topic, id);
                        }
                    }
                }
                ControlMessage::Hello { .. } | ControlMessage::Complete { .. } => {
                    // Server → client only. A client sending either is
                    // unusual but harmless; ignore it.
                }
            }
        } else {
            inner.dispatch_client_payload(&env.topic, Bytes::from(env.payload));
        }
    }

    // Withdraw from the lossless path first: a publisher awaiting this
    // connection must stop waiting on a socket that is going away. (Dropping
    // `outbound_tx` alone would not free it — the registry holds a clone.)
    for (topic, id) in lossless_ids.drain() {
        inner.remove_lossless_sub(&topic, id);
    }
    // Abort forwarders before dropping outbound_tx so no further frames
    // arrive at the writer; then let the writer drain and close the socket.
    for (_, h) in forwarders.drain() {
        h.abort();
    }
    drop(outbound_tx);
    let _ = writer.await;
}

/// Forward every frame from a broadcast receiver into the connection's
/// outbound mpsc. On `Lagged`, skip ahead (lossy — slow consumer does
/// not block the graph). Cloning `Bytes` is an Arc bump, not a copy.
///
/// This is the **lossy** path only. Under [`Delivery::Lossless`] the publisher
/// writes straight to the connection's outbound queue instead and never
/// broadcasts, so this task simply idles for the life of the connection.
async fn forward_broadcast(
    topic: String,
    mut rx: broadcast::Receiver<Bytes>,
    out: mpsc::Sender<Bytes>,
) {
    loop {
        match rx.recv().await {
            Ok(bytes) => match out.try_send(bytes) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::warn!("web_pub: client outbound full, dropping frame on '{topic}'");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => break,
            },
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!("web_pub: client lagged by {n} frames on '{topic}'");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn encode_control_frame(codec: CodecKind, ctrl: &ControlMessage) -> anyhow::Result<Bytes> {
    let payload = codec.encode(ctrl)?;
    let env = Envelope {
        topic: CONTROL_TOPIC.to_string(),
        time_ns: 0,
        payload,
    };
    Ok(Bytes::from(codec.encode(&env)?))
}

// ── TLS support (web-tls feature) ────────────────────────────────────────
//
// The plain-HTTP path uses `axum::serve` directly. For TLS we delegate to
// `axum-server`, which wraps each accepted connection with a tokio-rustls
// `TlsAcceptor` and then drives the same axum `Router` via hyper-util's
// `auto::Builder` — that builder supports HTTP/1.1 upgrades, so the
// existing `/ws` WebSocket handler works unchanged over `wss://`.
//
// We build the rustls `ServerConfig` ourselves (rather than
// `RustlsConfig::from_pem_file`) so we can pin the `ring` provider
// explicitly, matching the FIX adapter and avoiding any reliance on a
// process-wide installed default. Cert + key are read synchronously in
// [`WebServerBuilder::start`] so a misconfiguration shows up before the
// graph runs.

#[cfg(feature = "web-tls")]
fn load_tls_config(paths: &TlsPaths) -> anyhow::Result<axum_server::tls_rustls::RustlsConfig> {
    use std::fs::File;
    use std::io::BufReader;

    use rustls::ServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let cert_file = File::open(&paths.cert_path)
        .with_context(|| format!("web-tls: open cert {}", paths.cert_path.display()))?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<_, _>>()
        .with_context(|| format!("web-tls: parse cert {}", paths.cert_path.display()))?;
    if certs.is_empty() {
        anyhow::bail!(
            "web-tls: no certificates found in {}",
            paths.cert_path.display()
        );
    }

    let key_file = File::open(&paths.key_path)
        .with_context(|| format!("web-tls: open key {}", paths.key_path.display()))?;
    let mut key_reader = BufReader::new(key_file);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .with_context(|| format!("web-tls: parse key {}", paths.key_path.display()))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "web-tls: no private key found in {}",
                paths.key_path.display()
            )
        })?;

    let server_config =
        ServerConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
            .with_safe_default_protocol_versions()
            .context("web-tls: rustls protocol versions")?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("web-tls: build rustls ServerConfig")?;

    Ok(axum_server::tls_rustls::RustlsConfig::from_config(
        Arc::new(server_config),
    ))
}

#[cfg(feature = "web-tls")]
async fn serve_tls(
    listener: TcpListener,
    app: Router,
    config: axum_server::tls_rustls::RustlsConfig,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        let _ = shutdown_rx.await;
        // 5 s mirrors what most browsers wait before reconnecting; long
        // enough for in-flight WS frames to drain, short enough that
        // graph teardown isn't gated on a wedged client.
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });

    if let Err(e) = axum_server::from_tcp_rustls(listener, config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
    {
        log::warn!("web: axum-server (TLS) exited with error: {e}");
    }
}
