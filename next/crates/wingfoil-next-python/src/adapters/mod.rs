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
    feature = "etcd"
))]
pub mod common;

#[cfg(feature = "etcd")]
pub mod etcd;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis;
