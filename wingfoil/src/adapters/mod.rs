//! A library of input and output adapters

#[cfg(any(feature = "aeron", feature = "aeron-rs-beta"))]
pub mod aeron;
#[cfg(feature = "augurs")]
pub mod augurs;
#[cfg(feature = "kdb")]
pub mod cache;
/// Shared helpers reusable across I/O adapters (e.g. the out-of-window row
/// filter for historical reads). Always compiled so any adapter can use it
/// without touching feature gates.
pub mod common;
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
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "prometheus")]
pub mod prometheus;
#[cfg(feature = "redis")]
pub mod redis;
// Time-slicing logic for time-partitioned database reads. Currently used by the
// postgres adapter; the kdb adapter keeps its own copy in `kdb::read`.
#[cfg(feature = "postgres")]
pub(crate) mod time_slice;
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "zmq")]
pub mod zmq;
