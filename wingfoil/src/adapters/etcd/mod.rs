//! etcd adapter — streaming reads (watch) and writes (put) for etcd key-value store.
//!
//! Provides two graph nodes:
//!
//! - [`etcd_sub`] — producer that emits a key-prefix snapshot followed by live watch events
//! - [`etcd_pub`] — consumer that writes key-value pairs to etcd, with optional lease support
//!
//! # Setup
//!
//! ## Local (Docker)
//!
//! ```sh
//! docker run --rm -p 2379:2379 \
//!   -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
//!   -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
//!   gcr.io/etcd-development/etcd:v3.5.0
//! ```
//!
//! ## Kubernetes
//!
//! Deploy etcd as a `StatefulSet` and expose it via a `ClusterIP` service:
//!
//! ```yaml
//! apiVersion: apps/v1
//! kind: StatefulSet
//! metadata:
//!   name: etcd
//! spec:
//!   selector:
//!     matchLabels:
//!       app: etcd
//!   serviceName: etcd
//!   replicas: 1
//!   template:
//!     metadata:
//!       labels:
//!         app: etcd
//!     spec:
//!       containers:
//!         - name: etcd
//!           image: gcr.io/etcd-development/etcd:v3.5.0
//!           ports:
//!             - containerPort: 2379
//!           env:
//!             - name: ETCD_LISTEN_CLIENT_URLS
//!               value: http://0.0.0.0:2379
//!             - name: ETCD_ADVERTISE_CLIENT_URLS
//!               value: http://etcd:2379
//! ---
//! apiVersion: v1
//! kind: Service
//! metadata:
//!   name: etcd
//! spec:
//!   selector:
//!     app: etcd
//!   ports:
//!     - port: 2379
//!       targetPort: 2379
//! ```
//!
//! Connect from within the cluster using the service DNS name:
//!
//! ```ignore
//! let conn = EtcdConnection::new("http://etcd:2379");
//! ```
//!
//! For a cluster with multiple replicas, pass all endpoints:
//!
//! ```ignore
//! let conn = EtcdConnection::with_endpoints([
//!     "http://etcd-0.etcd:2379",
//!     "http://etcd-1.etcd:2379",
//!     "http://etcd-2.etcd:2379",
//! ]);
//! ```
//!
//! # Subscribing to a key prefix
//!
//! [`etcd_sub`] first emits a snapshot of all keys matching the prefix as
//! [`EtcdEventKind::Put`] events, then streams live watch events (puts and deletes).
//! The watch is registered *before* the GET, so no writes are missed during the handoff.
//!
//! ```ignore
//! use wingfoil::adapters::etcd::*;
//! use wingfoil::*;
//!
//! let conn = EtcdConnection::new("http://localhost:2379");
//!
//! etcd_sub(conn, "/config/")
//!     .collapse()
//!     .for_each(|event, _| {
//!         println!("{:?} {} = {:?}", event.kind, event.entry.key, event.entry.value_str())
//!     })
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Publishing key-value pairs
//!
//! [`etcd_pub`] (or the fluent `.etcd_pub()` method) consumes a
//! `Burst<EtcdEntry>` stream and writes each entry to etcd.
//!
//! ```ignore
//! use wingfoil::adapters::etcd::*;
//! use wingfoil::*;
//!
//! let conn = EtcdConnection::new("http://localhost:2379");
//!
//! // Write a fixed set of keys once, then stop.
//! constant(burst![
//!     EtcdEntry { key: "/config/host".into(), value: b"localhost".to_vec() },
//!     EtcdEntry { key: "/config/port".into(), value: b"8080".to_vec() },
//! ])
//! .etcd_pub(conn, None, true)
//! .run(RunMode::RealTime, RunFor::Cycles(1))
//! .unwrap();
//! ```
//!
//! ## Leases
//!
//! Pass a TTL to attach an etcd lease. Keys are automatically kept alive while the
//! graph is running and revoked immediately on clean shutdown:
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil::adapters::etcd::*;
//! use wingfoil::*;
//!
//! let conn = EtcdConnection::new("http://localhost:2379");
//!
//! // Keys expire 30 s after the consumer shuts down (or immediately on clean shutdown).
//! ticker(Duration::from_secs(10))
//!     .map(|_| burst![EtcdEntry { key: "/heartbeat".into(), value: b"ok".to_vec() }])
//!     .etcd_pub(conn, Some(Duration::from_secs(30)), true)
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Round-trip example
//!
//! See [`examples/etcd`](https://github.com/search?q=examples/etcd) for a full working
//! example that seeds keys with `etcd_pub`, watches them with `etcd_sub`, transforms
//! the values, and writes the results to a second prefix.
//!
//! ```ignore
//! use wingfoil::adapters::etcd::*;
//! use wingfoil::*;
//!
//! const SOURCE: &str = "/example/source/";
//! const DEST: &str = "/example/dest/";
//!
//! let conn = EtcdConnection::new("http://localhost:2379");
//!
//! let seed = constant(burst![
//!     EtcdEntry { key: format!("{SOURCE}greeting"), value: b"hello".to_vec() },
//!     EtcdEntry { key: format!("{SOURCE}subject"),  value: b"world".to_vec() },
//! ])
//! .etcd_pub(conn.clone(), None, true);
//!
//! let round_trip = etcd_sub(conn.clone(), SOURCE)
//!     .map(|burst| {
//!         burst.into_iter().map(|event| {
//!             let upper = event.entry.value_str().unwrap_or("").to_uppercase().into_bytes();
//!             EtcdEntry { key: event.entry.key.replacen(SOURCE, DEST, 1), value: upper }
//!         })
//!         .collect::<Burst<EtcdEntry>>()
//!     })
//!     .etcd_pub(conn, None, true);
//!
//! Graph::new(vec![seed, round_trip], RunMode::RealTime, RunFor::Cycles(3)).run().unwrap();
//! ```

mod read;
mod write;

#[cfg(all(test, feature = "etcd-integration-test"))]
mod integration_tests;

pub use read::*;
pub use write::*;

/// Connection configuration for etcd.
#[derive(Debug, Clone)]
pub struct EtcdConnection {
    /// etcd endpoints, e.g. `["http://localhost:2379"]`.
    pub endpoints: Vec<String>,
}

impl EtcdConnection {
    /// Create a connection config with a single endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoints: vec![endpoint.into()],
        }
    }

    /// Create a connection config with multiple endpoints (for an etcd cluster).
    pub fn with_endpoints(endpoints: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            endpoints: endpoints.into_iter().map(Into::into).collect(),
        }
    }
}

/// A single key-value pair from etcd.
#[derive(Debug, Clone, Default)]
pub struct EtcdEntry {
    pub key: String,
    pub value: Vec<u8>,
}

impl EtcdEntry {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }
}

/// The type of change represented by an [`EtcdEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EtcdEventKind {
    /// A key was created or updated.
    #[default]
    Put,
    /// A key was deleted.
    ///
    /// The associated [`EtcdEvent::entry`] carries the deleted key but an **empty value**
    /// (`value == vec![]`). Use [`EtcdEventKind::Put`] events if you need the value.
    Delete,
}

/// An event from an etcd watch stream.
///
/// Snapshot events (from the initial GET) are always [`EtcdEventKind::Put`].
/// Subsequent watch events reflect the actual change type from etcd.
#[derive(Debug, Clone, Default)]
pub struct EtcdEvent {
    pub kind: EtcdEventKind,
    pub entry: EtcdEntry,
    /// The etcd cluster revision at which this event was observed.
    pub revision: i64,
}
