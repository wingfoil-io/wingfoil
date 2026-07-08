//! A library of input and output adapters

#[cfg(any(feature = "aeron", feature = "aeron-rs-beta"))]
pub mod aeron;
#[cfg(feature = "kdb")]
pub mod cache;
// Shared adapter helpers (e.g. the out-of-window row filter). Add your adapter's
// feature to this gate when it becomes the first non-kdb consumer.
#[cfg(feature = "kdb")]
pub(crate) mod common;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
#[cfg(feature = "fix")]
#[doc(hidden)]
pub mod fix;
#[cfg(feature = "fluvio")]
pub mod fluvio;
#[cfg(feature = "iceoryx2")]
#[doc(hidden)]
pub mod iceoryx2;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "otlp")]
pub mod otlp;
#[cfg(feature = "prometheus")]
pub mod prometheus;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "zmq")]
pub mod zmq;
