//! Per-adapter Python bindings — the `#[pyadapter]` exposure of the
//! `wingfoil_next::adapters::*` I/O adapters.
//!
//! Each adapter lives behind a cargo feature of the same name, which turns on
//! the matching `wingfoil-next` adapter feature, so a wheel only carries the
//! adapters it was built with (`maturin develop -F postgres`). The generated
//! `#[pyfunction]`s are registered in the `wingfoil_next` `#[pymodule]` under
//! the same `#[cfg]`.
//!
//! The binding is a thin, *dynamic* skin over the natively-typed Rust adapter:
//! only the Python-facing edge erases (a row becomes a `dict`, a burst a
//! `list`), while the adapter's interior stays the concrete Rust record type,
//! exactly as [`crate::graph`] describes.

// Compiled whenever any adapter binding is, since that is its only consumer.
#[cfg(any(
    feature = "postgres",
    feature = "kafka",
    feature = "redis",
    feature = "etcd",
    feature = "fluvio",
    feature = "csv",
    feature = "zmq",
    feature = "otlp",
    feature = "augurs",
    feature = "kdb",
    feature = "fix"
))]
pub mod common;

#[cfg(feature = "augurs")]
pub mod augurs;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
#[cfg(feature = "fix")]
pub mod fix;
#[cfg(feature = "fluvio")]
pub mod fluvio;
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
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "zmq")]
pub mod zmq;
