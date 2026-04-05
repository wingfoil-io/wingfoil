//! Registry traits and implementations for ZMQ service discovery.
//!
//! Provides a backend-agnostic [`ZmqRegistry`] trait that decouples ZMQ from
//! any specific name-resolution mechanism:
//!
//! - [`EtcdRegistry`] — stores addresses in etcd under a lease
//!   (requires the `etcd` feature)

use anyhow::Result;

// ============ Traits ============

/// Backend-agnostic service registry for ZMQ service discovery.
///
/// Implemented by [`SeedRegistry`] and (when the `etcd` feature is enabled)
/// [`EtcdRegistry`].
pub trait ZmqRegistry: Send + 'static {
    /// Register `address` under `name`. Returns a handle whose [`ZmqHandle::revoke`]
    /// method cleans up the registration (e.g., removes the key, revokes a lease).
    fn register(&self, name: &str, address: &str) -> Result<Box<dyn ZmqHandle>>;
    /// Resolve `name` to a ZMQ address string (e.g., `"tcp://host:5556"`).
    fn lookup(&self, name: &str) -> Result<String>;
}

/// Handle returned by [`ZmqRegistry::register`]. Call [`revoke`](Self::revoke)
/// to clean up the registration on clean shutdown.
pub trait ZmqHandle: Send {
    /// Revoke the registration. Called from `stop()`; errors are logged, not propagated.
    fn revoke(&mut self);
}

// ============ Config types ============

/// Passed to [`ZeroMqPub::zmq_pub`] / [`ZeroMqPub::zmq_pub_on`].
///
/// Constructed via `From` impls — pass `()` for no registration, or
/// `("name", registry)` to register with a backend.
pub struct ZmqPubRegistration(pub(crate) Option<(String, Box<dyn ZmqRegistry>)>);

impl From<()> for ZmqPubRegistration {
    fn from(_: ()) -> Self {
        ZmqPubRegistration(None)
    }
}

impl<R: ZmqRegistry + 'static> From<(&str, R)> for ZmqPubRegistration {
    fn from((name, registry): (&str, R)) -> Self {
        ZmqPubRegistration(Some((name.to_string(), Box::new(registry))))
    }
}

/// Passed to [`zmq_sub`](super::zmq_sub).
///
/// Constructed via `From` impls — pass a `&str`/`String` for a direct address,
/// or `("name", registry)` for discovery-based lookup.
pub struct ZmqSubConfig(pub(crate) ZmqSubResolution);

pub(crate) enum ZmqSubResolution {
    Direct(String),
    Discover(String, Box<dyn ZmqRegistry>),
}

impl From<&str> for ZmqSubConfig {
    fn from(addr: &str) -> Self {
        ZmqSubConfig(ZmqSubResolution::Direct(addr.to_string()))
    }
}

impl From<String> for ZmqSubConfig {
    fn from(addr: String) -> Self {
        ZmqSubConfig(ZmqSubResolution::Direct(addr))
    }
}

impl From<&String> for ZmqSubConfig {
    fn from(addr: &String) -> Self {
        ZmqSubConfig(ZmqSubResolution::Direct(addr.clone()))
    }
}

impl<R: ZmqRegistry + 'static> From<(&str, R)> for ZmqSubConfig {
    fn from((name, registry): (&str, R)) -> Self {
        ZmqSubConfig(ZmqSubResolution::Discover(
            name.to_string(),
            Box::new(registry),
        ))
    }
}

// ============ EtcdRegistry ============

#[cfg(feature = "etcd")]
pub use etcd_impl::EtcdRegistry;

#[cfg(feature = "etcd")]
mod etcd_impl {
    use super::{ZmqHandle, ZmqRegistry};
    use crate::adapters::etcd::EtcdConnection;
    use anyhow::Result;
    use etcd_client::{Client, PutOptions};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Duration;
    use tokio::sync::Notify;

    const LEASE_TTL_SECS: i64 = 30;
    const KEEPALIVE_INTERVAL_SECS: u64 = 10;

    /// Service registry backed by etcd.
    ///
    /// Publishers write their address to an etcd key under a lease. The lease
    /// expires ~30 s after a crash (no keepalive renewal); clean shutdown revokes
    /// the lease immediately. Subscribers do a one-shot GET at construction time.
    ///
    /// ```ignore
    /// use wingfoil::adapters::zmq::EtcdRegistry;
    ///
    /// // Single endpoint (URL string)
    /// stream.zmq_pub(5556, ("quotes", EtcdRegistry::new("http://etcd:2379")));
    ///
    /// // etcd cluster
    /// stream.zmq_pub(5556, ("quotes", EtcdRegistry::with_endpoints([
    ///     "http://etcd-0.etcd:2379",
    ///     "http://etcd-1.etcd:2379",
    /// ])));
    /// ```
    pub struct EtcdRegistry {
        conn: EtcdConnection,
    }

    impl EtcdRegistry {
        /// Create an `EtcdRegistry` from a single endpoint URL or an [`EtcdConnection`].
        pub fn new(conn: impl Into<EtcdConnection>) -> Self {
            Self { conn: conn.into() }
        }

        /// Create an `EtcdRegistry` connected to multiple etcd endpoints (cluster).
        pub fn with_endpoints(endpoints: impl IntoIterator<Item = impl Into<String>>) -> Self {
            Self {
                conn: EtcdConnection::with_endpoints(endpoints),
            }
        }
    }

    struct EtcdHandle {
        lease_id: i64,
        conn: EtcdConnection,
        shutdown: Arc<Notify>,
        keepalive_thread: Option<JoinHandle<()>>,
    }

    impl ZmqHandle for EtcdHandle {
        fn revoke(&mut self) {
            self.shutdown.notify_one();
            if let Some(t) = self.keepalive_thread.take() {
                let _ = t.join();
            }
            revoke_lease(self.lease_id, &self.conn);
        }
    }

    impl ZmqRegistry for EtcdRegistry {
        fn register(&self, name: &str, address: &str) -> Result<Box<dyn ZmqHandle>> {
            let handle = etcd_register(name, address, &self.conn)?;
            Ok(Box::new(handle))
        }

        fn lookup(&self, name: &str) -> Result<String> {
            etcd_lookup(name, &self.conn)
        }
    }

    fn etcd_register(name: &str, address: &str, conn: &EtcdConnection) -> Result<EtcdHandle> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let lease_id = rt.block_on(async {
            let mut client = Client::connect(&conn.endpoints, None)
                .await
                .map_err(|e| anyhow::anyhow!("etcd connect failed: {e}"))?;
            let lease_resp = client
                .lease_grant(LEASE_TTL_SECS, None)
                .await
                .map_err(|e| anyhow::anyhow!("etcd lease_grant failed: {e}"))?;
            let id = lease_resp.id();
            let opts = PutOptions::new().with_lease(id);
            client
                .put(name, address, Some(opts))
                .await
                .map_err(|e| anyhow::anyhow!("etcd put failed: {e}"))?;
            Ok::<i64, anyhow::Error>(id)
        })?;

        let shutdown = Arc::new(Notify::new());
        let shutdown_clone = shutdown.clone();
        let endpoints = conn.endpoints.clone();

        let keepalive_thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("etcd keepalive runtime");
            rt.block_on(async {
                let mut client = match Client::connect(&endpoints, None).await {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("etcd keepalive connect failed: {e}");
                        return;
                    }
                };
                let (mut keeper, mut ka_stream) = match client.lease_keep_alive(lease_id).await {
                    Ok(pair) => pair,
                    Err(e) => {
                        log::error!("etcd lease_keep_alive failed: {e}");
                        return;
                    }
                };
                loop {
                    tokio::select! {
                        _ = shutdown_clone.notified() => break,
                        _ = tokio::time::sleep(Duration::from_secs(KEEPALIVE_INTERVAL_SECS)) => {}
                    }
                    if keeper.keep_alive().await.is_err() {
                        break;
                    }
                    match ka_stream.message().await {
                        Ok(Some(_)) => {}
                        _ => break,
                    }
                }
            });
        });

        Ok(EtcdHandle {
            lease_id,
            conn: conn.clone(),
            shutdown,
            keepalive_thread: Some(keepalive_thread),
        })
    }

    fn etcd_lookup(name: &str, conn: &EtcdConnection) -> Result<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let mut client = Client::connect(&conn.endpoints, None)
                .await
                .map_err(|e| anyhow::anyhow!("etcd connect failed: {e}"))?;
            let resp = client
                .get(name, None)
                .await
                .map_err(|e| anyhow::anyhow!("etcd get failed: {e}"))?;
            match resp.kvs().first() {
                Some(kv) => {
                    let addr = kv
                        .value_str()
                        .map_err(|e| anyhow::anyhow!("etcd value is not valid UTF-8: {e}"))?
                        .to_string();
                    Ok(addr)
                }
                None => Err(anyhow::anyhow!(
                    "no publisher named {:?} found in etcd",
                    name
                )),
            }
        })
    }

    fn revoke_lease(lease_id: i64, conn: &EtcdConnection) {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log::error!("etcd revoke runtime failed: {e}");
                return;
            }
        };
        rt.block_on(async {
            let mut client = match Client::connect(&conn.endpoints, None).await {
                Ok(c) => c,
                Err(e) => {
                    log::error!("etcd revoke connect failed: {e}");
                    return;
                }
            };
            if let Err(e) = client.lease_revoke(lease_id).await {
                log::error!("etcd lease_revoke failed: {e}");
            }
        });
    }
}
