//! ZMQ adapter — real-time pub/sub messaging using ØMQ sockets with optional
//! etcd-based service discovery.
//!
//! Provides two graph primitives:
//!
//! - [`zmq_sub`] — subscriber that connects to a ZMQ PUB socket
//! - [`ZeroMqPub::zmq_pub`] — publisher that binds a ZMQ PUB socket
//!
//! # Setup
//!
//! ZMQ is peer-to-peer — no broker process is required. The `zmq-beta` feature
//! bundles `libzmq` at build time, so no system installation is needed.
//!
//! Enable the feature in `Cargo.toml`:
//!
//! ```toml
//! wingfoil = { version = "...", features = ["zmq-beta"] }
//! ```
//!
//! # Direct pub/sub
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil::adapters::zmq::{ZeroMqPub, zmq_sub};
//! use wingfoil::*;
//!
//! // Publisher — binds on 127.0.0.1:5556
//! ticker(Duration::from_millis(100))
//!     .count()
//!     .zmq_pub(5556, ())
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//!
//! // Subscriber — direct address
//! let (data, status) = zmq_sub::<Vec<u8>>("tcp://localhost:5556")?;
//! data.for_each(|burst, _| {
//!     for msg in burst { println!("{msg:?}"); }
//! })
//! .run(RunMode::RealTime, RunFor::Forever)
//! .unwrap();
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # etcd-based discovery
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil::adapters::zmq::{EtcdRegistry, ZeroMqPub, zmq_sub};
//! use wingfoil::*;
//!
//! // Publisher registers its address in etcd under "quotes".
//! std::thread::spawn(move || {
//!     ticker(Duration::from_millis(100))
//!         .count()
//!         .zmq_pub(5556, ("quotes", EtcdRegistry::new("http://etcd:2379")))
//!         .run(RunMode::RealTime, RunFor::Forever)
//!         .unwrap();
//! });
//!
//! // Subscriber looks up the publisher address from etcd.
//! let (data, _status) = zmq_sub::<u64>(("quotes", EtcdRegistry::new("http://etcd:2379")))?;
//! data.for_each(|burst, _| println!("{burst:?}"))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! # Ok::<(), anyhow::Error>(())
//! ```

mod read;
pub mod registry;
mod write;

#[cfg(all(test, feature = "zmq-beta-integration-test"))]
mod integration_tests;

pub use read::*;
pub use registry::{ZmqHandle, ZmqPubRegistration, ZmqRegistry, ZmqSubConfig};
pub use write::*;

#[cfg(feature = "etcd")]
pub use registry::EtcdRegistry;

/// Connection status reported by the subscriber socket monitor.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ZmqStatus {
    Connected,
    #[default]
    Disconnected,
}

/// Internal per-message event wrapping either data or a status change.
#[derive(Debug, Clone)]
pub enum ZmqEvent<T> {
    Data(T),
    Status(ZmqStatus),
}

impl<T: Default> Default for ZmqEvent<T> {
    fn default() -> Self {
        ZmqEvent::Data(T::default())
    }
}
