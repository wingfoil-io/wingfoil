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

pub mod buffer;
pub mod error;
pub mod status;
pub(crate) mod transport;

mod pub_node;
mod sub_spin;
mod sub_threaded;

#[cfg(feature = "aeron-rusteron")]
pub mod rusteron_backend;

#[cfg(feature = "aeron-rs")]
pub mod aeron_rs_backend;

pub use buffer::{ClaimBuffer, FragmentBuffer, FragmentHeader};
pub use error::TransportError;
pub use pub_node::AeronPub;
pub use status::AeronStatus;

#[cfg(feature = "aeron-rusteron")]
pub use rusteron_backend::AeronHandle;

#[cfg(feature = "aeron-rs")]
pub use aeron_rs_backend::AeronRsHandle;

use crate::{Burst, Element, IntoStream, Stream};
use std::rc::Rc;

#[cfg(all(test, feature = "aeron-integration-test"))]
pub(crate) mod integration_tests;

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
// Fragment poll limit
// ---------------------------------------------------------------------------

/// Default cap on fragments processed per `poll()` cycle.
///
/// `256` is the Aeron sample harness convention (`aeron.sample.frameCountLimit=256`,
/// per the Aeron Java Programming Guide). The value is a **cap, not a target**:
/// `poll()` returns control after at most this many fragments OR when no more are
/// immediately available, whichever comes first.
///
/// **Trade-off:**
/// - Lower values (e.g. `32`) reduce per-cycle latency tail but add per-fragment
///   loop overhead.
/// - Higher values (e.g. `1024`) amortise loop overhead but lengthen the worst-case
///   `poll()` cycle.
///
/// `256` is the Schelling point used by every Aeron client tutorial.
pub const DEFAULT_FRAGMENT_LIMIT: usize = 256;

/// Subscriber options bag passed to [`aeron_sub_with_options`].
///
/// Every field is causally wired to runtime behaviour:
/// - `mode` selects [`AeronMode::Spin`] or [`AeronMode::Threaded`] node construction.
/// - `fragment_limit` flows through to the backend's per-`poll()` cap.
///
/// Construct via `AeronSubOptions::default()` (yields `mode: Spin`, `fragment_limit: 256`)
/// or with explicit fields.
#[derive(Debug, Clone, Copy)]
pub struct AeronSubOptions {
    /// Polling strategy — see [`AeronMode`].
    pub mode: AeronMode,
    /// Cap on fragments delivered per `poll()` — see [`DEFAULT_FRAGMENT_LIMIT`].
    pub fragment_limit: usize,
}

impl Default for AeronSubOptions {
    fn default() -> Self {
        Self {
            mode: AeronMode::Spin,
            fragment_limit: DEFAULT_FRAGMENT_LIMIT,
        }
    }
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
///
/// The fragment-poll cap defaults to [`DEFAULT_FRAGMENT_LIMIT`]. To tune it,
/// use [`aeron_sub_with_options`] or call `.with_fragment_limit(n)` on the
/// concrete subscriber handle before passing it in.
#[must_use]
pub fn aeron_sub<T, F, B>(subscriber: B, parser: F, mode: AeronMode) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    F: FnMut(&[u8]) -> Option<T> + Send + 'static,
    B: transport::AeronSubscriberBackend,
{
    aeron_sub_with_options(
        subscriber,
        parser,
        AeronSubOptions {
            mode,
            fragment_limit: DEFAULT_FRAGMENT_LIMIT,
        },
    )
}

/// Create an Aeron subscriber stream with explicit options.
///
/// Both fields of [`AeronSubOptions`] flow through to runtime behaviour:
/// `opts.mode` selects spin vs threaded node construction; `opts.fragment_limit`
/// is applied to the backend via [`AeronSubscriberBackend::with_fragment_limit`]
/// before the node is built.
#[must_use]
pub fn aeron_sub_with_options<T, F, B>(
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    F: FnMut(&[u8]) -> Option<T> + Send + 'static,
    B: transport::AeronSubscriberBackend,
{
    let subscriber = subscriber.with_fragment_limit(opts.fragment_limit);
    match opts.mode {
        AeronMode::Spin => sub_spin::AeronSpinSubNode::new(subscriber, parser).into_stream(),
        AeronMode::Threaded => sub_threaded::build(subscriber, parser),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_default_fragment_limit_constant_when_inspected_then_equals_256() {
        assert_eq!(DEFAULT_FRAGMENT_LIMIT, 256);
    }

    #[test]
    fn given_aeron_sub_options_when_default_then_fragment_limit_is_256() {
        let opts = AeronSubOptions::default();
        assert_eq!(opts.fragment_limit, DEFAULT_FRAGMENT_LIMIT);
        assert_eq!(opts.fragment_limit, 256);
    }

    #[test]
    fn given_aeron_sub_options_when_default_then_mode_is_spin() {
        assert!(matches!(AeronSubOptions::default().mode, AeronMode::Spin));
    }
}
