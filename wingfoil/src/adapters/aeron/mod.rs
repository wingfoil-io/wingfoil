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
//! | Feature         | Crate             | Notes                                           |
//! |-----------------|-------------------|-------------------------------------------------|
//! | `aeron`         | `rusteron-client` | C++ FFI. Recommended for production.            |
//! | `aeron-rs-beta` | `aeron-rs`        | Pure Rust, no C++ toolchain. **Beta — not for low-latency use, see warning below.** |
//!
//! The optional `aeron-driver` feature additionally embeds a media driver
//! (`rusteron-media-driver`) in-process; it implies `aeron`. Without it, point
//! the client at an externally-running media driver (the usual production
//! topology).
//!
//! ## ⚠️ `aeron-rs-beta` takes a lock on the graph thread
//!
//! The `aeron-rs` crate returns its `Subscription` / `Publication` handles as
//! `Arc<Mutex<…>>` and shares them with its own background client-conductor
//! thread, which locks them on every publisher connect/disconnect. The backend
//! therefore **acquires that `Mutex` on every `poll()` / `offer()`** — and in
//! [`AeronMode::Spin`] those calls run inside the graph `cycle()` on the graph
//! thread. That contended lock on the hot path is antithetical to Aeron's
//! lock-free design and violates wingfoil's "no locks in `cycle()`" invariant.
//!
//! Consequences and guidance:
//! - For latency-sensitive or production workloads, use the `aeron` (rusteron)
//!   backend, whose `poll()` / `offer()` are genuinely lock-free.
//! - If you must use `aeron-rs-beta`, prefer [`AeronMode::Threaded`] so the lock
//!   is taken on the background poll thread rather than the graph thread.
//! - The `aeron-rs-beta` publisher has no threaded mode; its `offer()` always
//!   locks on the calling (graph) thread. Treat it as functional-but-slow.
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

mod channel;
mod discovery;
mod external;
mod pub_node;
mod status_stream;
mod sub_fragment_node;
mod sub_spin;
mod sub_threaded;

#[cfg(feature = "aeron")]
pub mod rusteron_backend;

#[cfg(feature = "aeron-rs-beta")]
pub mod aeron_rs_backend;

pub use buffer::{ClaimBuffer, FragmentBuffer, FragmentHeader};
pub use channel::ChannelUri;
#[allow(deprecated)]
pub use discovery::aeron_sub_discover;
pub use discovery::{
    DiscoveryError, aeron_pub_named, aeron_sub_named, lookup_pub, lookup_sub, register_pub,
    register_sub,
};
pub use error::TransportError;
pub use external::ExternalSource;
pub use pub_node::AeronPub;
pub use status::AeronStatus;
pub use status_stream::AeronStatusStream;
pub use transport::{AeronPublisherBackend, AeronSubscriberBackend};

#[cfg(feature = "aeron")]
pub use rusteron_backend::AeronHandle;

#[cfg(feature = "aeron-rs-beta")]
pub use aeron_rs_backend::AeronRsHandle;

use crate::{Burst, Element, IntoStream, Stream};
use std::cell::RefCell;
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

// ---------------------------------------------------------------------------
// aeron_sub_fragment — typed-parser surface (parallel-additive)
// ---------------------------------------------------------------------------

/// Create an Aeron subscriber stream with a typed-parser surface.
///
/// Parallel-additive sibling of [`aeron_sub`]. Differs only in the parser
/// signature — both factories return `Rc<dyn Stream<Burst<T>>>`.
///
/// | Surface       | `aeron_sub` (bytes parser)                | `aeron_sub_fragment` (typed parser)                                    |
/// |---------------|-------------------------------------------|---------------------------------------------------------------------|
/// | Parser sig    | `FnMut(&[u8]) -> Option<T>`               | `FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError>`   |
/// | Header access | No                                        | Yes — `position`, `session_id`, `stream_id`                         |
/// | Parser error  | No (only `None` to skip)                  | Yes — `Err(TransportError::*)` is logged and the fragment dropped   |
///
/// `Ok(Some(v))` pushes `v` onto the burst, `Ok(None)` silently skips the
/// fragment, and `Err(_)` is logged at WARN and skipped — a malformed
/// fragment never aborts the cycle.
///
/// # Migration from `aeron_sub`
///
/// Wrap the existing parser body in `Ok(...)` and read bytes via
/// `frag.as_ref()`:
///
/// ```ignore
/// // before
/// aeron_sub(sub, |bytes: &[u8]| bytes.try_into().ok().map(i64::from_le_bytes), Spin);
/// // after
/// aeron_sub_fragment(
///     sub,
///     |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
///         Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
///     },
///     AeronSubOptions::default(),
/// );
/// ```
///
/// # The `Send` parser bound
///
/// The factory is mode-polymorphic (chooses spin or threaded at runtime from
/// `opts.mode`); the threaded variant moves the parser onto a background
/// thread, so the parser bound MUST be `Send` for the factory even though
/// the spin variant alone would not require it.
#[must_use]
pub fn aeron_sub_fragment<T, F, B>(
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    F: FnMut(&buffer::FragmentBuffer<'_>) -> Result<Option<T>, error::TransportError>
        + Send
        + 'static,
    B: transport::AeronSubscriberBackend,
{
    let subscriber = subscriber.with_fragment_limit(opts.fragment_limit);
    match opts.mode {
        AeronMode::Spin => {
            sub_fragment_node::AeronSpinSubFragmentNode::new(subscriber, parser).into_stream()
        }
        AeronMode::Threaded => sub_fragment_node::build_threaded(subscriber, parser),
    }
}

// ---------------------------------------------------------------------------
// aeron_sub_fragment_with_status — typed-parser surface with reactive status
// side-channel (Story 12.5)
// ---------------------------------------------------------------------------

/// Create an Aeron subscriber stream paired with a reactive
/// [`AeronStatusStream`].
///
/// Parallel-additive sibling of [`aeron_sub_fragment`]. Returns a tuple
/// `(data_stream, status_stream)`:
/// - `data_stream` is identical in shape to [`aeron_sub_fragment`]'s output.
/// - `status_stream` emits a `Burst<AeronStatus>` **only on transition
///   cycles** (no re-emission on steady state). Wire it as a `Dep::Active`
///   upstream and iterate `peek_ref()` for transitions, or call
///   `peek_value().last()` for a one-shot read.
///
/// # Status derivation order
///
/// After each successful `poll_fragments`, the new status is derived from
/// the backend in this order:
///
/// 1. `is_closed()` → [`AeronStatus::Closed`] (terminal — checked first)
/// 2. `is_connected()` → [`AeronStatus::Connected`]
/// 3. Otherwise → [`AeronStatus::Disconnected`]
///
/// A `poll_fragments` error short-circuits the cycle before the status is
/// recorded, so a transient I/O failure does not register a phantom
/// `Disconnected` transition.
///
/// # Threaded mode
///
/// `opts.mode == AeronMode::Threaded` propagates status too: the background
/// poll thread multiplexes data values and lifecycle-status transitions over
/// the single receiver channel (carried in-band so a `Connected` transition is
/// ordered before the fragments that followed it), and the data node demuxes
/// them into the same `(data, status)` pair. Status is sampled at the poll
/// thread's cadence (which backs off when idle), so a transition surfaces
/// within roughly one poll interval rather than per graph cycle as in spin
/// mode.
#[must_use]
pub fn aeron_sub_fragment_with_status<T, F, B>(
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> (Rc<dyn Stream<Burst<T>>>, Rc<dyn Stream<Burst<AeronStatus>>>)
where
    T: Element + Send,
    F: FnMut(&buffer::FragmentBuffer<'_>) -> Result<Option<T>, error::TransportError>
        + Send
        + 'static,
    B: transport::AeronSubscriberBackend,
{
    let subscriber = subscriber.with_fragment_limit(opts.fragment_limit);
    match opts.mode {
        AeronMode::Spin => {
            // The spin subscriber records status on the graph thread; wire it as
            // the status node's active upstream so transitions are forwarded once
            // per producer tick (no always_callback busy-spin).
            let status = Rc::new(RefCell::new(status_stream::AeronStatusStream::default()));
            let status_stream: Rc<dyn Stream<Burst<AeronStatus>>> = status.clone();
            let data = sub_fragment_node::AeronSpinSubFragmentNode::with_status(
                subscriber,
                parser,
                Rc::clone(&status),
            )
            .into_stream();
            status
                .borrow_mut()
                .set_producer(Rc::downgrade(&data.clone().as_node()));
            (data, status_stream)
        }
        AeronMode::Threaded => {
            // Threaded variant: the background poll thread multiplexes data and
            // lifecycle-status transitions over the single receiver channel
            // (see `AeronItem`). The data node demuxes them, replaying each
            // status transition into the shared `AeronStatusStream`. We wire the
            // status node's producer to the data node exactly as the spin path
            // does, so transitions are forwarded once per tick.
            let status = Rc::new(RefCell::new(status_stream::AeronStatusStream::default()));
            let status_stream: Rc<dyn Stream<Burst<AeronStatus>>> = status.clone();
            let data = sub_fragment_node::build_threaded_with_status(
                subscriber,
                parser,
                Rc::clone(&status),
            );
            status
                .borrow_mut()
                .set_producer(Rc::downgrade(&data.clone().as_node()));
            (data, status_stream)
        }
    }
}

// ---------------------------------------------------------------------------
// aeron_sub_fragment_named — name-checked factory wrapper around aeron_sub_fragment
// (Story 12.6)
// ---------------------------------------------------------------------------

/// Create an Aeron subscriber stream after verifying `name` is registered in
/// the discovery layer.
///
/// Wraps [`aeron_sub_fragment`] with a [`discovery::aeron_sub_named`] guard: the
/// name is validated and looked up before any stream construction happens.
/// On unknown / empty / invalid names, returns `Err(DiscoveryError::*)`
/// before constructing the underlying node.
///
/// See [`aeron_sub_fragment`] for the data-stream contract (parser signature,
/// `Burst<T>` shape, fragment limit) and [`discovery::aeron_sub_named`] for
/// the validation contract.
///
/// # Errors
///
/// Returns [`discovery::DiscoveryError`] if the name fails validation or has
/// not been registered via [`discovery::register_sub`].
#[must_use = "the returned subscriber stream must be wired into a graph or its fragments are silently dropped"]
pub fn aeron_sub_fragment_named<T, F, B>(
    name: &str,
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> Result<Rc<dyn Stream<Burst<T>>>, discovery::DiscoveryError>
where
    T: Element + Send,
    F: FnMut(&buffer::FragmentBuffer<'_>) -> Result<Option<T>, error::TransportError>
        + Send
        + 'static,
    B: transport::AeronSubscriberBackend,
{
    let subscriber = discovery::aeron_sub_named(name, subscriber)?;
    Ok(aeron_sub_fragment(subscriber, parser, opts))
}

// ---------------------------------------------------------------------------
// aeron_sub_fragment_with_status_named — name-checked factory wrapper around
// aeron_sub_fragment_with_status (Story 12.6)
// ---------------------------------------------------------------------------

/// Create an Aeron subscriber stream paired with a reactive
/// [`AeronStatusStream`], after verifying `name` is registered.
///
/// Wraps [`aeron_sub_fragment_with_status`] with a [`discovery::aeron_sub_named`]
/// guard. See [`aeron_sub_fragment_with_status`] for the data + status-stream
/// contract (including the threaded-mode caveat that the status stream stays
/// in its `Default` state) and [`discovery::aeron_sub_named`] for the
/// validation contract.
///
/// # Errors
///
/// Returns [`discovery::DiscoveryError`] if the name fails validation or has
/// not been registered via [`discovery::register_sub`].
#[must_use = "the returned (data, status) stream pair must be wired into a graph or its fragments are silently dropped"]
pub fn aeron_sub_fragment_with_status_named<T, F, B>(
    name: &str,
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> Result<(Rc<dyn Stream<Burst<T>>>, Rc<dyn Stream<Burst<AeronStatus>>>), discovery::DiscoveryError>
where
    T: Element + Send,
    F: FnMut(&buffer::FragmentBuffer<'_>) -> Result<Option<T>, error::TransportError>
        + Send
        + 'static,
    B: transport::AeronSubscriberBackend,
{
    let subscriber = discovery::aeron_sub_named(name, subscriber)?;
    Ok(aeron_sub_fragment_with_status(subscriber, parser, opts))
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

    #[test]
    fn given_aeron_sub_fragment_with_status_when_threaded_mode_then_disconnected_emits_no_transition()
     {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;
        use crate::{NodeOperators, RunFor, RunMode, StreamOperators};
        use std::time::Duration;

        // The default MockSubscriber is never connected, so the threaded poll
        // thread derives `Disconnected` — which equals the stream's default —
        // and sends no transition. A brief real run must therefore leave the
        // status stream empty (no spurious transitions).
        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
        };
        let (_data, status_stream) = aeron_sub_fragment_with_status(
            backend,
            parser,
            AeronSubOptions {
                mode: AeronMode::Threaded,
                fragment_limit: DEFAULT_FRAGMENT_LIMIT,
            },
        );
        let collected = status_stream.collect();
        collected
            .clone()
            .run(
                RunMode::RealTime,
                RunFor::Duration(Duration::from_millis(100)),
            )
            .unwrap();
        assert!(
            collected
                .peek_value()
                .into_iter()
                .all(|burst| burst.value.is_empty()),
            "disconnected backend must not emit any status transition"
        );
    }

    // -----------------------------------------------------------------
    // Story 12.6: factory wrapper tests (aeron_sub_fragment_named +
    // aeron_sub_fragment_with_status_named).
    //
    // Each test uses a unique registry name to avoid cross-test
    // contamination of the process-global registry (PUB_REGISTRY /
    // SUB_REGISTRY in discovery.rs).
    // -----------------------------------------------------------------

    #[test]
    fn given_aeron_sub_fragment_named_when_registered_then_returns_stream() {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::discovery::register_sub;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;

        register_sub("test-named-sub-1", "aeron:ipc".to_string(), 1001).unwrap();
        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |_f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> { Ok(None) };
        let stream = aeron_sub_fragment_named(
            "test-named-sub-1",
            backend,
            parser,
            AeronSubOptions::default(),
        )
        .expect("registered name resolves to a stream");
        assert!(stream.peek_value().is_empty());
    }

    #[test]
    fn given_aeron_sub_fragment_named_when_unregistered_then_returns_unknown_error() {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::discovery::DiscoveryError;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;

        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |_f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> { Ok(None) };
        let err = aeron_sub_fragment_named(
            "test-never-registered-2",
            backend,
            parser,
            AeronSubOptions::default(),
        )
        .expect_err("unregistered name returns Unknown");
        assert!(matches!(err, DiscoveryError::Unknown(_)));
    }

    #[test]
    fn given_aeron_sub_fragment_named_when_empty_name_then_returns_empty_name_error() {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::discovery::DiscoveryError;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;

        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |_f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> { Ok(None) };
        let err = aeron_sub_fragment_named("", backend, parser, AeronSubOptions::default())
            .expect_err("empty name returns EmptyName before stream construction");
        assert_eq!(err, DiscoveryError::EmptyName);
    }

    #[test]
    fn given_aeron_sub_fragment_named_when_invalid_name_then_returns_invalid_name_error() {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::discovery::DiscoveryError;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;

        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |_f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> { Ok(None) };
        let err = aeron_sub_fragment_named(
            "contains space",
            backend,
            parser,
            AeronSubOptions::default(),
        )
        .expect_err("invalid name returns InvalidName");
        assert!(matches!(err, DiscoveryError::InvalidName(_)));
    }

    #[test]
    fn given_aeron_sub_fragment_with_status_named_when_registered_then_returns_pair() {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::discovery::register_sub;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;

        register_sub("test-named-sub-5", "aeron:ipc".to_string(), 1005).unwrap();
        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |_f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> { Ok(None) };
        let (data_stream, status_stream) = aeron_sub_fragment_with_status_named(
            "test-named-sub-5",
            backend,
            parser,
            AeronSubOptions::default(),
        )
        .expect("registered name resolves to a stream pair");
        assert!(data_stream.peek_value().is_empty());
        assert!(status_stream.peek_value().is_empty());
    }

    #[test]
    fn given_aeron_sub_fragment_with_status_named_when_unregistered_then_returns_unknown_error() {
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::discovery::DiscoveryError;
        use crate::adapters::aeron::error::TransportError;
        use crate::adapters::aeron::transport::MockSubscriber;

        let backend = MockSubscriber::from_messages(vec![]);
        let parser = |_f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> { Ok(None) };
        let err = aeron_sub_fragment_with_status_named(
            "test-never-registered-6",
            backend,
            parser,
            AeronSubOptions::default(),
        )
        .expect_err("unregistered name returns Unknown");
        assert!(matches!(err, DiscoveryError::Unknown(_)));
    }
}
