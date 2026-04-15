//! HTTP + WebSocket server used by the `web` adapter.
//!
//! [`WebServer`] binds a TCP port synchronously (so bind errors surface
//! before the graph starts) and spawns an axum server on its own dedicated
//! tokio runtime — the same pattern used by the Prometheus exporter.
//! Graph nodes (`web_pub`, `web_sub`) register topics with the server at
//! construction time; the server stays alive for the lifetime of the
//! [`WebServer`] handle (or until [`WebServer::stop`] is called).

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Context as _;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tower_http::services::ServeDir;

use super::codec::{CONTROL_TOPIC, Codec, ControlMessage, Envelope};

/// Per-topic broadcast capacity for server → client publishes. Slow
/// consumers that cannot drain at this rate receive
/// [`broadcast::error::RecvError::Lagged`] instead of blocking the graph.
const PUBLISH_BROADCAST_CAPACITY: usize = 1024;

/// Per-connection WS outbound queue depth. Bounded so a slow socket
/// cannot grow memory without bound.
const CONNECTION_OUTBOUND_CAPACITY: usize = 1024;

/// Per-subscribed-topic mpsc capacity (client → graph). Bounded so a
/// misbehaving client cannot grow memory without bound.
const SUBSCRIBE_MPSC_CAPACITY: usize = 1024;

/// A binary frame on its way to one or more WebSocket connections.
type SharedFrame = Arc<Vec<u8>>;

pub(crate) struct WebServerInner {
    pub(crate) codec: Codec,
    /// Topics the graph publishes. Each WS connection that subscribes
    /// to `topic` gets its own broadcast receiver.
    pub(crate) pub_topics: Mutex<HashMap<String, broadcast::Sender<SharedFrame>>>,
    /// Topics the graph consumes from the browser. When a client frame
    /// arrives on one of these topics we forward the raw payload bytes
    /// to every registered mpsc sender. There is usually one sender per
    /// `web_sub::<T>()` call.
    pub(crate) sub_topics: Mutex<HashMap<String, Vec<mpsc::Sender<Vec<u8>>>>>,
}

impl WebServerInner {
    fn new(codec: Codec) -> Self {
        Self {
            codec,
            pub_topics: Mutex::new(HashMap::new()),
            sub_topics: Mutex::new(HashMap::new()),
        }
    }

    /// Get (or create) the broadcast sender for a publish topic.
    pub(crate) fn get_or_create_pub_topic(&self, topic: &str) -> broadcast::Sender<SharedFrame> {
        let mut guard = self.pub_topics.lock().expect("pub_topics lock poisoned");
        guard
            .entry(topic.to_string())
            .or_insert_with(|| broadcast::channel(PUBLISH_BROADCAST_CAPACITY).0)
            .clone()
    }

    /// Register an mpsc sender as a listener for a subscribe topic.
    pub(crate) fn register_sub_sender(&self, topic: &str, tx: mpsc::Sender<Vec<u8>>) {
        let mut guard = self.sub_topics.lock().expect("sub_topics lock poisoned");
        guard.entry(topic.to_string()).or_default().push(tx);
    }

    /// Forward a raw payload received from a WS client to every
    /// registered sub listener on this topic. Drops listeners whose
    /// mpsc is closed.
    fn dispatch_client_payload(&self, topic: &str, payload: Vec<u8>) {
        let mut guard = self.sub_topics.lock().expect("sub_topics lock poisoned");
        if let Some(senders) = guard.get_mut(topic) {
            senders.retain(|tx| {
                // try_send: drop newest under overload to protect graph latency.
                match tx.try_send(payload.clone()) {
                    Ok(()) => true,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        log::warn!("web_sub: topic '{topic}' listener overloaded — dropping frame");
                        true
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => false,
                }
            });
        }
    }
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
    /// If `true`, the server is a no-op because the graph is running in
    /// historical mode (set by [`WebServerBuilder::start_historical`]).
    historical_noop: bool,
}

impl WebServer {
    /// Start configuring a new server that will bind to `addr`.
    ///
    /// Use `"127.0.0.1:0"` to let the OS assign a port, then read it
    /// back with [`WebServer::port`] after [`WebServerBuilder::start`].
    pub fn bind(addr: impl Into<String>) -> WebServerBuilder {
        WebServerBuilder {
            addr: addr.into(),
            codec: Codec::BINCODE,
            static_dir: None,
        }
    }

    /// The port the server bound on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The codec the server is using.
    pub fn codec(&self) -> Codec {
        self.inner.codec
    }

    /// True when the server was created as a historical-mode no-op.
    pub fn is_historical_noop(&self) -> bool {
        self.historical_noop
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
    codec: Codec,
    static_dir: Option<PathBuf>,
}

impl WebServerBuilder {
    /// Set the wire codec (default: bincode).
    pub fn codec(mut self, codec: impl Into<Codec>) -> Self {
        self.codec = codec.into();
        self
    }

    /// Serve static files from `dir` under `GET /` alongside the
    /// WebSocket endpoint. Useful for hosting the `wingfoil-js` UI
    /// bundle from the same origin.
    pub fn serve_static(mut self, dir: impl Into<PathBuf>) -> Self {
        self.static_dir = Some(dir.into());
        self
    }

    /// Bind the TCP listener and spawn the HTTP + WS server.
    ///
    /// Binding is synchronous so a port conflict is reported
    /// immediately, before the graph starts.
    pub fn start(self) -> anyhow::Result<WebServer> {
        let listener =
            TcpListener::bind(&self.addr).with_context(|| format!("web: bind to {}", self.addr))?;
        let port = listener.local_addr().context("web: local_addr")?.port();
        let inner = Arc::new(WebServerInner::new(self.codec));
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
                    listener
                        .set_nonblocking(true)
                        .expect("listener set_nonblocking");
                    let tokio_listener =
                        tokio::net::TcpListener::from_std(listener).expect("web: from_std");
                    let app = build_router(inner_clone, static_dir);
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
        })
    }

    /// Build a no-op server for historical-mode runs. No TCP port is
    /// bound; all publishes and subscribes become no-ops.
    pub fn start_historical(self) -> anyhow::Result<WebServer> {
        Ok(WebServer {
            inner: Arc::new(WebServerInner::new(self.codec)),
            port: 0,
            shutdown_tx: None,
            thread: None,
            historical_noop: true,
        })
    }
}

/// Build the axum router: `GET /ws` upgrades to a WebSocket, and
/// optionally `GET /*` serves static files.
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

/// Per-connection task. Manages two directions:
///
/// - **outbound** (server → client): one mpsc queue with a writer task
///   draining into the WS sink. Forwarders (one per subscribed pub
///   topic) push into this queue.
/// - **inbound** (client → server): the reader task decodes every
///   envelope and either handles a control message or dispatches the
///   payload to the matching sub_topics listeners.
async fn handle_socket(socket: WebSocket, inner: Arc<WebServerInner>) {
    let codec = inner.codec;
    let (mut ws_sink, mut ws_stream) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(CONNECTION_OUTBOUND_CAPACITY);

    // Writer task — drains outbound mpsc into the WS sink.
    let writer = tokio::spawn(async move {
        while let Some(bytes) = outbound_rx.recv().await {
            if ws_sink.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_sink.close().await;
    });

    // Send Hello control frame.
    let hello_env = match encode_control(
        codec,
        ControlMessage::Hello {
            codec: codec.kind(),
            version: super::codec::wire_version(),
        },
    ) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::error!("web: encode hello failed: {e}");
            writer.abort();
            return;
        }
    };
    if outbound_tx.send(hello_env).await.is_err() {
        return;
    }

    // Track per-topic forwarder tasks spawned in response to Subscribe frames.
    let mut forwarders: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

    // Reader loop.
    while let Some(msg) = ws_stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                log::debug!("web: ws recv error: {e}");
                break;
            }
        };
        let bytes: Vec<u8> = match msg {
            Message::Binary(b) => b.into(),
            Message::Text(t) => t.as_bytes().to_vec(),
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        let env = match codec.decode_envelope(&bytes) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("web: bad envelope from client: {e}");
                continue;
            }
        };
        if env.topic == CONTROL_TOPIC {
            let ctrl: ControlMessage = match codec.decode_payload(&env.payload) {
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
                    }
                }
                ControlMessage::Hello { .. } => {
                    // Clients sending Hello is unusual but harmless.
                }
            }
        } else {
            inner.dispatch_client_payload(&env.topic, env.payload);
        }
    }

    // Tear down forwarders before dropping outbound_tx so pending frames drain.
    for (_, h) in forwarders.drain() {
        h.abort();
    }
    drop(outbound_tx);
    let _ = writer.await;
}

/// Forward every frame from a broadcast receiver into the connection's
/// outbound mpsc. On `Lagged`, skip ahead (lossy — slow consumer does
/// not block the graph).
async fn forward_broadcast(
    topic: String,
    mut rx: broadcast::Receiver<SharedFrame>,
    out: mpsc::Sender<Vec<u8>>,
) {
    loop {
        match rx.recv().await {
            Ok(bytes) => {
                // try_send: if outbound is full, drop the frame. A
                // permanent backlog would block the broadcast channel.
                match out.try_send((*bytes).clone()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        log::warn!("web_pub: client outbound full, dropping frame on '{topic}'");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!("web_pub: client lagged by {n} frames on '{topic}'");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn encode_control(codec: Codec, ctrl: ControlMessage) -> anyhow::Result<Vec<u8>> {
    let payload = codec.encode_payload(&ctrl)?;
    let env = Envelope {
        topic: CONTROL_TOPIC.to_string(),
        time_ns: 0,
        payload,
    };
    codec.encode_envelope(&env)
}

/// Construct a SocketAddr from a host/port for callers that prefer typed
/// addresses. Exposed for completeness; [`WebServer::bind`] accepts any
/// `Into<String>` already.
pub fn addr(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let s = format!("{host}:{port}");
    s.parse().with_context(|| format!("web: parse addr {s}"))
}

// Re-export broadcast/mpsc capacity constants for documentation + tests.
#[allow(dead_code)]
pub(crate) mod capacities {
    pub const PUBLISH_BROADCAST: usize = super::PUBLISH_BROADCAST_CAPACITY;
    pub const CONNECTION_OUTBOUND: usize = super::CONNECTION_OUTBOUND_CAPACITY;
    pub const SUBSCRIBE_MPSC: usize = super::SUBSCRIBE_MPSC_CAPACITY;
}

// Small helper so the `SUBSCRIBE_MPSC_CAPACITY` constant is used where
// the public API creates the channel.
pub(crate) fn subscribe_channel<T>() -> (mpsc::Sender<T>, mpsc::Receiver<T>) {
    mpsc::channel(SUBSCRIBE_MPSC_CAPACITY)
}
