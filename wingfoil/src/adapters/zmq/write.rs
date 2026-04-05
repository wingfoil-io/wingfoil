use std::rc::Rc;

use super::registry::{ZmqHandle, ZmqPubRegistration};
use crate::channel::Message;
use crate::{Element, GraphState, IntoNode, MutableNode, Node, RunMode, Stream, UpStreams};
use serde::Serialize;

struct ZeroMqSenderNode<T: Element + Send + Serialize> {
    src: Rc<dyn Stream<T>>,
    port: u16,
    bind_address: String,
    registration: ZmqPubRegistration,
    socket: Option<zmq::Socket>,
    registry_handle: Option<Box<dyn ZmqHandle>>,
}

const FLAGS: i32 = 0;

impl<T: Element + Send + Serialize> MutableNode for ZeroMqSenderNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let value = self.src.peek_value();
        let msg = Message::build(value, state);
        let data = bincode::serialize(&msg)?;
        let sock = self
            .socket
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing socket"))?;
        sock.send(data, FLAGS)?;
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
            registry_handle: None,
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
            registry_handle: None,
        }
        .into_node()
    }
}
