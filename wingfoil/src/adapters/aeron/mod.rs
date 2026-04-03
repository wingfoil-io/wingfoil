//! Aeron IPC/UDP adapter for wingfoil.
//!
//! Provides two graph nodes and two polling modes:
//! - [`aeron_sub`] — source: polls an Aeron subscription and emits `Burst<T>`
//! - [`AeronPub`] — sink trait: serialises values and offers them to a publication
//!
//! # Backends
//!
//! Enable exactly one backend feature flag:
//!
//! | Feature            | Crate             | Notes                                    |
//! |--------------------|-------------------|------------------------------------------|
//! | `aeron-rusteron`   | `rusteron-client` | C++ FFI, recommended for production      |
//! | `aeron-rs`         | `aeron-rs`        | Pure Rust, no C++ toolchain required     |
//!
//! # Polling modes
//!
//! Pass an [`AeronMode`] to [`aeron_sub`] to select the strategy:
//!
//! - **[`AeronMode::Spin`]** *(primary)* — polls Aeron inside `cycle()` on the graph
//!   thread.  Zero thread-crossing latency; burns one CPU core.  Returns `Ok(true)`
//!   only when fragments arrive so downstream nodes tick reactively.
//!
//! - **[`AeronMode::Threaded`]** *(secondary)* — spins in a background thread,
//!   delivers via channel.  Frees the graph thread at the cost of one channel hop.
//!
//! # Setup
//!
//! The Aeron media driver must be running before graph execution:
//!
//! ```sh
//! # Standalone Java media driver (simplest for dev/test):
//! java -cp aeron-all-*.jar io.aeron.driver.MediaDriver
//! ```
//!
//! # Subscribing (spin mode — rusteron backend)
//!
//! ```ignore
//! use wingfoil::adapters::aeron::{AeronHandle, AeronMode, aeron_sub};
//! use wingfoil::*;
//! use std::time::Duration;
//!
//! let handle = AeronHandle::connect().unwrap();
//! let sub = handle.subscription("aeron:ipc", 1001, Duration::from_secs(5)).unwrap();
//!
//! aeron_sub(sub, |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes), AeronMode::Spin)
//!     .map(|burst| burst.into_iter().sum::<i64>())
//!     .print()
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Publishing
//!
//! ```ignore
//! use wingfoil::adapters::aeron::{AeronHandle, AeronPub};
//! use wingfoil::*;
//! use std::time::Duration;
//!
//! let handle = AeronHandle::connect().unwrap();
//! let pub_ = handle.publication("aeron:ipc", 1002, Duration::from_secs(5)).unwrap();
//!
//! ticker(std::time::Duration::from_millis(100))
//!     .count()
//!     .map(|n: u64| burst![n])
//!     .aeron_pub(pub_, |v: &u64| v.to_le_bytes().to_vec())
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```

pub(crate) mod transport;

mod pub_node;
mod sub_spin;
mod sub_threaded;

#[cfg(feature = "aeron-rusteron")]
pub mod rusteron_backend;

#[cfg(feature = "aeron-rs")]
pub mod aeron_rs_backend;

pub use pub_node::AeronPub;

#[cfg(feature = "aeron-rusteron")]
pub use rusteron_backend::AeronHandle;

#[cfg(feature = "aeron-rs")]
pub use aeron_rs_backend::AeronRsHandle;

use crate::{Burst, Element, IntoStream, Stream};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// AeronMode
// ---------------------------------------------------------------------------

/// Selects how the subscriber polls Aeron.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeronMode {
    /// Poll Aeron inside `cycle()` on the graph thread (primary pattern).
    ///
    /// Zero thread-crossing latency; consumes one CPU core.  Returns
    /// `Ok(true)` only when fragments arrive so downstream nodes tick
    /// reactively.
    Spin,

    /// Spin on Aeron in a dedicated background thread (secondary pattern).
    ///
    /// Adds one channel-hop of latency but frees the graph thread for other
    /// work.  Only `RunMode::RealTime` is supported.
    Threaded,
}

// ---------------------------------------------------------------------------
// aeron_sub — backend-agnostic factory
// ---------------------------------------------------------------------------

/// Create an Aeron subscriber stream.
///
/// # Arguments
///
/// - `subscriber` — obtained from [`AeronHandle::subscription`] or
///   [`AeronRsHandle::subscription`].
/// - `parser` — converts raw Aeron fragment bytes to `Option<T>`.
///   Return `None` to discard a fragment.
/// - `mode` — [`AeronMode::Spin`] or [`AeronMode::Threaded`].
#[must_use]
pub fn aeron_sub<T, F, B>(subscriber: B, parser: F, mode: AeronMode) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    F: FnMut(&[u8]) -> Option<T> + Send + 'static,
    B: transport::AeronSubscriberBackend,
{
    match mode {
        AeronMode::Spin => sub_spin::AeronSpinSubNode::new(subscriber, parser).into_stream(),
        AeronMode::Threaded => sub_threaded::build(subscriber, parser),
    }
}
