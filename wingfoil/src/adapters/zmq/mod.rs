//! ZMQ adapter — real-time pub/sub messaging using ØMQ sockets with optional
//! registry-based service discovery.
//!
//! Provides two graph primitives:
//!
//! - [`zmq_sub`] — subscriber that connects to a ZMQ PUB socket
//! - [`ZeroMqPub::zmq_pub`] — publisher that binds a ZMQ PUB socket
//!
//! Both accept an optional registry argument so the same functions work for
//! direct-address, seed-based, and etcd-based discovery.
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
//! # Subscribing
//!
//! ```ignore
//! use wingfoil::adapters::zmq::{zmq_sub, SeedRegistry};
//! use wingfoil::*;
//!
//! // Direct address
//! let (data, status) = zmq_sub::<Vec<u8>>("tcp://localhost:5556")?;
//!
//! // Seed-based discovery
//! let (data, status) = zmq_sub::<Vec<u8>>(("quotes", SeedRegistry::new(&["tcp://seed:7777"])))?;
//!
//! data.for_each(|burst, _| {
//!     for msg in burst { println!("{msg:?}"); }
//! })
//! .run(RunMode::RealTime, RunFor::Forever)
//! .unwrap();
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # Publishing
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil::adapters::zmq::{ZeroMqPub, SeedRegistry};
//! use wingfoil::*;
//!
//! // No registration — direct connect only
//! ticker(Duration::from_millis(100))
//!     .count()
//!     .zmq_pub(5556, ())
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Service Discovery
//!
//! Seed nodes act as lightweight name registries: publishers register on
//! startup, subscribers query at construction time. Data still flows
//! peer-to-peer over normal PUB/SUB sockets.
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil::adapters::zmq::{ZeroMqPub, SeedRegistry, zmq_sub, start_seed};
//! use wingfoil::*;
//!
//! // Start a seed on a well-known address (stops when dropped).
//! let _seed = start_seed("tcp://0.0.0.0:7777")?;
//!
//! let seeds = SeedRegistry::new(&["tcp://seed-host:7777"]);
//!
//! // Publisher registers its address with the seed.
//! std::thread::spawn(|| {
//!     ticker(Duration::from_millis(100))
//!         .count()
//!         .zmq_pub(5556, ("quotes", SeedRegistry::new(&["tcp://seed-host:7777"])))
//!         .run(RunMode::RealTime, RunFor::Forever)
//!         .unwrap();
//! });
//!
//! // Subscriber resolves the publisher by name — no hardcoded address.
//! let (data, _status) = zmq_sub::<u64>(("quotes", seeds))?;
//! data.for_each(|burst, _| println!("{burst:?}"))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! # Ok::<(), anyhow::Error>(())
//! ```

mod read;
pub mod registry;
pub mod seed;
mod write;

#[cfg(all(test, feature = "zmq-beta-integration-test"))]
mod integration_tests;

pub use read::*;
pub use registry::{SeedRegistry, ZmqHandle, ZmqPubRegistration, ZmqRegistry, ZmqSubConfig};
pub use seed::{SeedHandle, start_seed};
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
