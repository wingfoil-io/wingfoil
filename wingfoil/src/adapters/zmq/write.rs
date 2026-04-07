use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::registry::{ZmqHandle, ZmqPubRegistration};
use crate::channel::Message;
use crate::{Element, GraphState, IntoNode, MutableNode, Node, RunMode, Stream, UpStreams};
use serde::Serialize;

static MONITOR_ID: AtomicUsize = AtomicUsize::new(0);
const ZMQ_EVENT_ACCEPTED: u16 = 0x0008;

/// Maximum time to buffer messages while waiting for a subscriber to connect.
/// Messages buffered longer than this are discarded — a subscriber connecting
/// after this window should not receive stale data.
const BUFFER_TIMEOUT: Duration = Duration::from_millis(500);

struct ZeroMqSenderNode<T: Element + Send + Serialize> {
    src: Rc<dyn Stream<T>>,
    port: u16,
    bind_address: String,
    registration: ZmqPubRegistration,
    socket: Option<zmq::Socket>,
    monitor: Option<zmq::Socket>,
    registry_handle: Option<Box<dyn ZmqHandle>>,
    subscriber_connected: bool,
    accepted_at: Option<Instant>,
    buffer: Vec<Vec<u8>>,
    buffer_start: Option<Instant>,
}

const FLAGS: i32 = 0;

impl<T: Element + Send + Serialize> ZeroMqSenderNode<T> {
    fn check_monitor(&mut self) {
        let Some(monitor) = self.monitor.as_ref() else {
            return;
        };
        loop {
            let mut items = [monitor.as_poll_item(zmq::POLLIN)];
            let _ = zmq::poll(&mut items, 0); // non-blocking
            if !items[0].is_readable() {
                break;
            }
            if let Ok(event_frame) = monitor.recv_msg(0) {
                // Drain the address frame.
                while monitor.get_rcvmore().unwrap_or(false) {
                    let _ = monitor.recv_msg(0);
                }
                if event_frame.len() >= 2 {
                    let event_id = u16::from_le_bytes([event_frame[0], event_frame[1]]);
                    log::debug!("zmq pub monitor event: 0x{event_id:04X}");
                    if event_id == ZMQ_EVENT_ACCEPTED {
                        self.accepted_at = Some(Instant::now());
                    }
                }
            }
        }
    }
}

impl<T: Element + Send + Serialize> MutableNode for ZeroMqSenderNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if !self.subscriber_connected {
            self.check_monitor();
            // After TCP ACCEPTED, the subscriber still needs a brief moment
            // for its subscription filter to propagate through ZMQ internals.
            // We mark as connected once at least one cycle has elapsed since
            // the ACCEPTED event, giving the filter time to arrive.
            if let Some(accepted_at) = self.accepted_at
                && accepted_at.elapsed() >= Duration::from_millis(1)
            {
                self.subscriber_connected = true;
            }
        }

        let value = self.src.peek_value();
        let msg = Message::build(value, state);
        let data = bincode::serialize(&msg)?;
        let sock = self
            .socket
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing socket"))?;

        if self.subscriber_connected {
            // Flush buffered messages, then send current.
            for buffered in self.buffer.drain(..) {
                sock.send(buffered, FLAGS)?;
            }
            self.buffer_start = None;
            sock.send(data, FLAGS)?;
        } else {
            // No subscriber yet — buffer the message.
            let now = Instant::now();
            let start = *self.buffer_start.get_or_insert(now);
            if now.duration_since(start) > BUFFER_TIMEOUT {
                self.buffer.clear();
                self.buffer_start = Some(now);
            }
            self.buffer.push(data);
        }

        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.src.clone().as_node()], vec![])
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("ZMQ nodes only support real-time mode");
        }
        let context = zmq::Context::new();
        let socket = context.socket(zmq::SocketType::PUB)?;

        let monitor_id = MONITOR_ID.fetch_add(1, Ordering::Relaxed);
        let monitor_addr = format!("inproc://zmq-pub-monitor-{monitor_id}");
        socket.monitor(&monitor_addr, ZMQ_EVENT_ACCEPTED as i32)?;
        let monitor = context.socket(zmq::PAIR)?;
        monitor.connect(&monitor_addr)?;
        self.monitor = Some(monitor);

        let address = format!("tcp://{}:{}", self.bind_address, self.port);
        socket.bind(&address)?;
        self.socket = Some(socket);
        if let Some((name, registry)) = &self.registration.0 {
            self.registry_handle = Some(registry.register(name, &address)?);
        }
        Ok(())
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        if let Some(mut h) = self.registry_handle.take() {
            h.revoke();
        }
        let Some(sock) = self.socket.as_ref() else {
            return Ok(());
        };
        let msg: Message<T> = Message::EndOfStream;
        let data = bincode::serialize(&msg)?;
        sock.send(data, FLAGS)?;
        Ok(())
    }
}

/// Fluent API for attaching a ZMQ PUB socket to any stream.
///
/// Pass `()` for no registration, or `("name", registry)` to register with a
/// discovery backend:
///
/// ```ignore
/// // No registration — direct connect only
/// stream.zmq_pub(5556, ())
///
/// // Register with etcd
/// stream.zmq_pub(5556, ("quotes", EtcdRegistry::new(conn)))
/// ```
pub trait ZeroMqPub<T: Element + Send> {
    /// Bind on `127.0.0.1:port` and optionally register with a discovery backend.
    ///
    /// **Multi-host note:** `127.0.0.1` is a loopback address — it cannot be
    /// reached by subscribers on other machines. When using a registry (e.g.
    /// `EtcdRegistry`) in a multi-host deployment, use [`zmq_pub_on`](Self::zmq_pub_on)
    /// with a routable bind address so the stored address is reachable by remote
    /// subscribers.
    fn zmq_pub(&self, port: u16, registration: impl Into<ZmqPubRegistration>) -> Rc<dyn Node>;
    /// Bind on `address:port` and optionally register with a discovery backend.
    /// Use this in multi-host deployments where `127.0.0.1` is not reachable.
    fn zmq_pub_on(
        &self,
        address: &str,
        port: u16,
        registration: impl Into<ZmqPubRegistration>,
    ) -> Rc<dyn Node>;
}

impl<T: Element + Send + Serialize> ZeroMqPub<T> for Rc<dyn Stream<T>> {
    fn zmq_pub(&self, port: u16, registration: impl Into<ZmqPubRegistration>) -> Rc<dyn Node> {
        ZeroMqSenderNode {
            src: self.clone(),
            port,
            bind_address: "127.0.0.1".to_string(),
            registration: registration.into(),
            socket: None,
            monitor: None,
            registry_handle: None,
            subscriber_connected: false,
            accepted_at: None,
            buffer: Vec::new(),
            buffer_start: None,
        }
        .into_node()
    }

    fn zmq_pub_on(
        &self,
        address: &str,
        port: u16,
        registration: impl Into<ZmqPubRegistration>,
    ) -> Rc<dyn Node> {
        ZeroMqSenderNode {
            src: self.clone(),
            port,
            bind_address: address.to_string(),
            registration: registration.into(),
            socket: None,
            monitor: None,
            registry_handle: None,
            subscriber_connected: false,
            accepted_at: None,
            buffer: Vec::new(),
            buffer_start: None,
        }
        .into_node()
    }
}
