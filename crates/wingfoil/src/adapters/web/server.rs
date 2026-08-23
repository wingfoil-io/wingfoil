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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc, oneshot};
use tower_http::services::ServeDir;
use wingfoil::RunMode;

use super::codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION};

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
    /// Always lossless: the publisher paces itself to the slowest *live*
    /// subscriber, so every frame reaches every subscriber in order.
    ///
    /// With **no** subscribers the publisher never waits — frames are simply
    /// dropped on the floor as before, so a server-less or unwatched run does
    /// not hang. A merely slow client *does* stall the graph, deliberately:
    /// that is the whole point, and it is why this is not the real-time
    /// default.
    ///
    /// "Slowest" is bounded, though, because otherwise it would include *gone
    /// and never coming back*. A subscriber whose outbound queue is full and
    /// drains nothing for [`WebServerBuilder::lossless_stall_timeout`] is
    /// treated as dead and withdrawn — see that method for why a live client,
    /// however slow, never trips it.
    Lossless,
}

impl Delivery {
    /// Whether this policy delivers losslessly under `run_mode`. The whole of
    /// [`Delivery::Auto`]'s meaning lives here.
    fn resolves_lossless(self, run_mode: RunMode) -> bool {
        match (self, run_mode) {
            (Delivery::Lossy, _) => false,
            (Delivery::Lossless, _) => true,
            (Delivery::Auto, RunMode::RealTime) => false,
            (Delivery::Auto, RunMode::HistoricalFrom(_)) => true,
        }
    }
}

/// How long a lossless publish waits for a subscriber whose outbound queue is
/// full before giving up on it. See [`WebServerBuilder::lossless_stall_timeout`].
pub(crate) const DEFAULT_LOSSLESS_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One client's subscription to one publish topic: the connection's own
/// outbound queue, an id so the connection can withdraw exactly its own
/// registration on `Unsubscribe` or close, and the signal that closes the whole
/// connection when a lossless publisher gives up on it.
pub(crate) struct Sub {
    id: u64,
    tx: mpsc::Sender<Bytes>,
    close: Arc<Notify>,
}

/// A snapshot of one topic's subscribers, taken once per instant by
/// [`WebServerInner::targets`] and drained by [`WebServerInner::deliver_to`].
/// Each entry is `(id, outbound queue, connection close signal)`.
pub(crate) type Targets = Vec<(u64, mpsc::Sender<Bytes>, Arc<Notify>)>;

pub(crate) struct WebServerInner {
    pub(crate) codec: CodecKind,
    /// The configured delivery policy. [`Delivery::Auto`] is turned into a
    /// concrete one at graph start, and that resolution is stored **per
    /// publish**, not here — see `web_pub`'s `lossless` flag in `write.rs`.
    /// A server-wide cell would let two overlapping runs of different run
    /// modes overwrite each other's policy.
    pub(crate) delivery: Delivery,
    /// How long a lossless publish waits on a full outbound queue before
    /// treating that subscriber as dead.
    pub(crate) lossless_stall_timeout: Duration,
    /// Topics the graph publishes: per topic, one entry per subscribed
    /// connection, holding that connection's bounded outbound queue directly.
    /// Frames flow as refcounted [`Bytes`], so fanning one out to N clients is
    /// N refcount bumps rather than N copies.
    ///
    /// **There is one fan-out, not one per policy.** [`Delivery`] chooses how a
    /// frame is *offered* to each queue — [`Self::deliver_to`] drops on a full
    /// queue under lossy and awaits a slot under lossless — but both read this
    /// same registry, so [`WebServer::subscriber_count`] is a single source of
    /// truth and means the same thing in either mode.
    pub(crate) pub_topics: Mutex<HashMap<String, Vec<Sub>>>,
    /// Hands out [`Sub::id`]s — unique per server, so a connection can withdraw
    /// exactly its own subscription.
    next_sub_id: AtomicU64,
    /// Topics the graph consumes from the browser. When a client frame
    /// arrives on one of these topics we forward the raw payload bytes
    /// to every registered mpsc sender. There is usually one sender per
    /// `web_sub::<T>()` call.
    pub(crate) sub_topics: Mutex<HashMap<String, Vec<mpsc::Sender<Bytes>>>>,
}

impl WebServerInner {
    fn new(codec: CodecKind, delivery: Delivery, lossless_stall_timeout: Duration) -> Self {
        Self {
            codec,
            delivery,
            lossless_stall_timeout,
            pub_topics: Mutex::new(HashMap::new()),
            next_sub_id: AtomicU64::new(0),
            sub_topics: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve this server's configured [`Delivery`] against a run mode.
    ///
    /// The *result* is stored per publish rather than on the server: the run
    /// mode does not exist when the server is built, and a `WebServer` can
    /// serve more than one graph — including two at once, in different run
    /// modes, which a server-wide cell would let overwrite each other.
    pub(crate) fn resolve_delivery(&self, run_mode: RunMode) -> bool {
        self.delivery.resolves_lossless(run_mode)
    }

    /// Register a connection's outbound queue for `topic`, returning the id
    /// needed to withdraw it again. `close` is that connection's shutdown
    /// signal, used only by [`Self::deliver_to`] when a lossless publisher
    /// declares the subscriber dead.
    fn register_sub(&self, topic: &str, tx: mpsc::Sender<Bytes>, close: Arc<Notify>) -> u64 {
        let id = self.next_sub_id.fetch_add(1, Ordering::Relaxed);
        self.pub_topics
            .lock()
            .expect("pub_topics lock poisoned")
            .entry(topic.to_string())
            .or_default()
            .push(Sub { id, tx, close });
        id
    }

    /// Withdraw one connection's subscription to `topic`.
    fn remove_sub(&self, topic: &str, id: u64) {
        let mut guard = self.pub_topics.lock().expect("pub_topics lock poisoned");
        if let Some(subs) = guard.get_mut(topic) {
            subs.retain(|s| s.id != id);
            if subs.is_empty() {
                guard.remove(topic);
            }
        }
    }

    /// Snapshot the connections subscribed to `topic`.
    ///
    /// Taken **out of the mutex** so no lock is ever held across a suspension
    /// point, and taken once per instant rather than once per frame: from the
    /// publisher's side the set can only usefully change between instants, and
    /// a subscription that goes away mid-instant surfaces as a send failure,
    /// which [`Self::deliver_to`] already handles.
    pub(crate) fn targets(&self, topic: &str) -> Targets {
        self.pub_topics
            .lock()
            .expect("pub_topics lock poisoned")
            .get(topic)
            .map(|subs| {
                subs.iter()
                    .map(|s| (s.id, s.tx.clone(), s.close.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Offer `bytes` to each of `targets` under the policy in force, and
    /// withdraw any subscriber the attempt proves unreachable.
    ///
    /// An empty `targets` never waits and never allocates: a graph publishing
    /// with no clients connected drops the frame on the floor in both modes.
    ///
    /// * **Lossy** — `try_send`, dropping the frame for any client whose queue
    ///   is full. Nothing a client does can stall the graph, which is the
    ///   real-time contract: a tab that has fallen behind is already showing
    ///   stale data, and stalling a live system is worse than a hole in a chart.
    /// * **Lossless** — wait for a slot, so a slow socket paces the publisher
    ///   (and through it the graph) instead of losing frames. The waits run
    ///   **concurrently**, so the cost of an instant is the slowest subscriber
    ///   rather than the sum of all of them — and one wedged client costs one
    ///   timeout window in total, not one per client.
    ///
    /// Two things withdraw a subscriber — from `targets`, so the rest of this
    /// instant skips it, and from the registry, so later instants do too:
    ///
    /// * its queue is **closed** — the socket died and the writer task ended;
    /// * (lossless only) its queue is **full and stays full** for
    ///   [`WebServerBuilder::lossless_stall_timeout`]. Pacing to the slowest
    ///   subscriber has to mean the slowest *live* one: a half-open peer (a
    ///   dead machine, a partitioned network, a closed laptop lid) never closes
    ///   its socket, so without this the graph would park until TCP keepalive
    ///   — hours — and, because the end-of-run `Complete` goes out the same
    ///   way, `run()` itself would hang after the last cycle.
    ///
    /// A **stalled** subscriber additionally has its whole connection closed,
    /// and that is not tidiness — it is the difference between a client that
    /// recovers and one that hangs forever. Withdrawing only from the registry
    /// would leave the socket open and silent: no further data frames, and no
    /// end-of-run `Complete` either. `@wingfoil/client` keys both `onComplete`
    /// and its stop-reconnecting logic on that marker, so the client would
    /// neither complete nor retry. The clients this strands are exactly the
    /// *recoverable* ones — the closed laptop lid that suspends, trips the
    /// bound, wakes and drains its queue — since a genuinely dead peer notices
    /// nothing either way. Closing lets it reconnect on its own — and the close
    /// must read as *abnormal* to do that: teardown aborts the writer with no
    /// Close frame, so the client sees 1006 and retries, where a clean close
    /// (1000/1001) tells `@wingfoil/client` the session is done and stops its
    /// reconnect loop.
    ///
    /// Closing the connection rather than the one subscription is deliberate:
    /// every topic on a connection shares one outbound queue, so a stall is a
    /// connection-level condition and the other topics are equally stuck.
    pub(crate) async fn deliver_to(
        &self,
        topic: &str,
        targets: &mut Targets,
        bytes: Bytes,
        lossless: bool,
    ) {
        if targets.is_empty() {
            return;
        }
        let withdrawn: Vec<u64> = if lossless {
            let timeout = self.lossless_stall_timeout;
            let sends = targets.iter().map(|(id, tx, close)| {
                let bytes = bytes.clone();
                async move {
                    match tx.send_timeout(bytes, timeout).await {
                        Ok(()) => None,
                        Err(mpsc::error::SendTimeoutError::Closed(_)) => Some(*id),
                        Err(mpsc::error::SendTimeoutError::Timeout(_)) => {
                            log::warn!(
                                "web_pub: subscriber on '{topic}' accepted nothing for \
                                 {timeout:?} — treating it as gone, closing its connection \
                                 so it can reconnect"
                            );
                            // `notify_one` (not `notify_waiters`) so the signal
                            // is remembered if the connection task is not parked
                            // on it at this instant.
                            close.notify_one();
                            Some(*id)
                        }
                    }
                }
            });
            futures::future::join_all(sends)
                .await
                .into_iter()
                .flatten()
                .collect()
        } else {
            targets
                .iter()
                .filter_map(|(id, tx, _)| match tx.try_send(bytes.clone()) {
                    Ok(()) => None,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        log::warn!("web_pub: client outbound full, dropping frame on '{topic}'");
                        None
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => Some(*id),
                })
                .collect()
        };
        if !withdrawn.is_empty() {
            targets.retain(|(id, ..)| !withdrawn.contains(id));
            for id in withdrawn {
                self.remove_sub(topic, id);
            }
        }
    }

    /// [`Self::deliver_to`] for a single frame that is not part of an instant —
    /// the end-of-run `Complete` marker.
    pub(crate) async fn deliver(&self, topic: &str, bytes: Bytes, lossless: bool) {
        let mut targets = self.targets(topic);
        self.deliver_to(topic, &mut targets, bytes, lossless).await;
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
            lossless_stall_timeout: DEFAULT_LOSSLESS_STALL_TIMEOUT,
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

    /// The [`WebServerBuilder::lossless_stall_timeout`] the server was built
    /// with.
    pub fn lossless_stall_timeout(&self) -> Duration {
        self.inner.lossless_stall_timeout
    }

    /// True when the server was created as a historical-mode no-op.
    pub fn is_historical_noop(&self) -> bool {
        self.historical_noop
    }

    /// How many connected clients a publish on `topic` would currently reach.
    ///
    /// A client's `Subscribe` is processed asynchronously by its connection's
    /// reader task, so a client that has *sent* one is not yet receiving: a
    /// publisher fans out to the subscribers registered at that moment, and a
    /// frame published before then is dropped, not queued. This counts exactly
    /// that registry, so it steps from 0 to 1 when a publish would start being
    /// delivered — which makes it the thing to wait on before publishing a
    /// short, finite sequence. Because there is one registry for both delivery
    /// policies, the count means the same thing in either mode.
    pub fn subscriber_count(&self, topic: &str) -> usize {
        self.inner
            .pub_topics
            .lock()
            .expect("pub_topics lock poisoned")
            .get(topic)
            .map_or(0, Vec::len)
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
    lossless_stall_timeout: Duration,
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

    /// How long a lossless publish waits on a subscriber whose outbound queue
    /// is full before treating it as gone (default: 30 s). No effect under
    /// [`Delivery::Lossy`], which never waits at all.
    ///
    /// This bounds *death*, not slowness. The wait is for one **slot** in a
    /// 1024-deep queue, so a live client trips it only by draining nothing at
    /// all for the whole window — which a browser doing anything, however
    /// slowly, does not. What does trip it is a half-open peer: a dead machine
    /// or a partitioned network leaves the socket open with no one behind it,
    /// and without a bound the graph would park until TCP keepalive notices,
    /// which is hours.
    ///
    /// Shorten it to make a stalled subscriber surface faster (the tests run
    /// at a few hundred milliseconds); lengthen it if a legitimate client can
    /// go quiet for minutes at a time and you would rather wait than drop it.
    pub fn lossless_stall_timeout(mut self, timeout: Duration) -> Self {
        self.lossless_stall_timeout = timeout;
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

        let inner = Arc::new(WebServerInner::new(
            self.codec,
            self.delivery,
            self.lossless_stall_timeout,
        ));
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
            inner: Arc::new(WebServerInner::new(
                self.codec,
                self.delivery,
                self.lossless_stall_timeout,
            )),
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
/// drained by a writer task; publishers write into it directly via the
/// fan-out registry (`register_sub` hands them this connection's sender).
/// Inbound (client → server) is handled inline in the reader loop below.
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

    // Raised by `deliver_to` when a lossless publisher gives up on this
    // connection. The reader below selects on it, so the connection tears down
    // and the socket closes — which is what lets a client that comes back (a
    // suspended laptop) notice the drop and reconnect, instead of holding a
    // socket that will never carry another frame.
    let close = Arc::new(Notify::new());

    // One id per subscribed topic, so this connection can withdraw exactly its
    // own registrations. There is a single fan-out registry for both delivery
    // policies, so there is nothing else to keep in step with it.
    let mut sub_ids: HashMap<String, u64> = HashMap::new();

    // True when a lossless publisher declared this client dead, which changes
    // how the connection tears down below.
    let mut stalled = false;

    loop {
        let next = tokio::select! {
            biased;
            () = close.notified() => {
                stalled = true;
                break;
            }
            next = ws_stream.next() => next,
        };
        let Some(msg) = next else { break };
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
                        if sub_ids.contains_key(&topic) {
                            continue;
                        }
                        // The publisher writes straight into this connection's
                        // outbound queue, so registering it *is* subscribing —
                        // there is no per-subscription relay task to spawn.
                        let id = inner.register_sub(&topic, outbound_tx.clone(), close.clone());
                        sub_ids.insert(topic, id);
                    }
                }
                ControlMessage::Unsubscribe { topics } => {
                    for topic in topics {
                        if let Some(id) = sub_ids.remove(&topic) {
                            inner.remove_sub(&topic, id);
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

    // Withdraw before dropping `outbound_tx`: a publisher awaiting this
    // connection must stop waiting on a socket that is going away, and dropping
    // the sender alone would not free it — the registry holds a clone.
    for (topic, id) in sub_ids.drain() {
        inner.remove_sub(&topic, id);
    }
    drop(outbound_tx);
    if stalled {
        // The writer is parked on a socket that has accepted nothing for the
        // stall timeout, so awaiting its drain would hang this task forever and
        // never drop the `WebSocket` — leaving the connection open, which is the
        // whole thing being fixed. Abort instead: both split halves drop, the
        // socket closes with no Close frame, and the client sees an abnormal
        // drop (1006) — the kind its reconnect logic retries, where a clean
        // close would tell it the session is over.
        writer.abort();
    } else {
        // Normal close: let the writer drain so a queued `Complete` still goes
        // out before the socket closes.
        let _ = writer.await;
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
