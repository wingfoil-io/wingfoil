//! aeron adapter — the Aeron IPC/UDP low-latency message transport. It ports
//! the classic `wingfoil::adapters::aeron` module onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Sources** — the free builder functions [`aeron_sub_fragment`] and
//!   [`aeron_sub_fragment_with_status`] on a
//!   [`GraphBuilder`](crate::fluent::GraphBuilder): poll an Aeron subscription
//!   and emit `Stream<Burst<T>>` (plus, for the `_with_status` twin, a paired
//!   `Stream<Burst<AeronStatus>>` of lifecycle transitions).
//! - **Sink** — the [`AeronSinkOps`] extension trait on `Stream<Burst<T>>`
//!   ([`aeron_pub`](AeronSinkOps::aeron_pub) /
//!   [`aeron_pub_with_status`](AeronSinkOps::aeron_pub_with_status)), enabled
//!   with `use wingfoil_next::adapters::aeron::AeronSinkOps;`.
//!
//! Like classic (and unlike the networked async adapters) Aeron is synchronous
//! and poll-based, so the feature deliberately does **not** pull in
//! `async`/tokio.
//!
//! # Backends
//!
//! Enable exactly one backend feature flag:
//!
//! | Feature    | Crate             | Notes                                          |
//! |------------|-------------------|------------------------------------------------|
//! | `aeron`    | `rusteron-client` | C++ FFI. Recommended for production.           |
//! | `aeron-rs` | `aeron-rs`        | Pure Rust, no C++ toolchain. **Experimental — not for low-latency use, see the warning below.** |
//!
//! The optional `aeron-driver` feature additionally embeds a media driver
//! (`rusteron-media-driver`) in-process; it implies `aeron`. Without it, point
//! the client at an externally-running media driver (the usual production
//! topology).
//!
//! ## ⚠️ `aeron-rs` takes a lock on the graph thread
//!
//! The `aeron-rs` crate returns its `Subscription` / `Publication` handles as
//! `Arc<Mutex<…>>` and shares them with its own background client-conductor
//! thread, which locks them on every publisher connect/disconnect. The backend
//! therefore **acquires that `Mutex` on every `poll()` / `offer()`**. To keep
//! that lock off the graph cycle, the subscriber backend reports
//! [`supports_graph_thread_poll`](AeronSubscriberBackend::supports_graph_thread_poll)
//! `= false`, and the source factories **automatically downgrade a requested
//! [`AeronMode::Spin`] to [`AeronMode::Threaded`]** for it (logging a warning) —
//! so its lock can never land on the graph thread. `Spin` is unreachable for the
//! `aeron-rs` subscriber by construction.
//!
//! Consequences and guidance:
//! - For latency-sensitive or production workloads, use the `aeron` (rusteron)
//!   backend, whose `poll()` / `offer()` are genuinely lock-free and run on the
//!   graph thread in [`AeronMode::Spin`] without a lock.
//! - The `aeron-rs` **publisher** has no threaded mode; its `offer()` always
//!   locks on the calling (graph) thread. It remains functional-but-slow and
//!   there is no automatic downgrade for the publish side — avoid it on
//!   latency-sensitive paths.
//!
//! # Polling modes
//!
//! Pass an [`AeronMode`] (via [`AeronSubOptions`]) to select the strategy:
//!
//! - **[`AeronMode::Spin`]** *(primary)* — polls Aeron inside the cycle on the
//!   graph thread, via a busy-spin
//!   [`custom_node`](crate::fluent::GraphBuilder::custom_node). Zero
//!   thread-crossing latency; burns one CPU core. Ticks only when fragments
//!   arrive (or a status transition is observed), so downstream stays reactive.
//! - **[`AeronMode::Threaded`]** *(secondary)* — polls on a dedicated background
//!   thread feeding the [`channel`](crate::fluent::SourceOps::channel) layer,
//!   with an exponential idle back-off. Frees the graph thread at the cost of
//!   one channel hop. Real-time only.
//!
//! `fragment_limit` caps fragments processed per `poll()`
//! ([`DEFAULT_FRAGMENT_LIMIT`] = 256).
//!
//! # Status side-channel
//!
//! [`aeron_sub_fragment_with_status`] and
//! [`AeronSinkOps::aeron_pub_with_status`] are parallel-additive siblings: they
//! return a tuple whose second half is a `Stream<Burst<AeronStatus>>` emitting
//! **only on transition** (never re-emitting a steady state). Status is derived
//! after each successful poll/offer in a fixed order — `is_closed()` →
//! [`AeronStatus::Closed`] (terminal, checked first), `is_connected()` →
//! [`AeronStatus::Connected`], else [`AeronStatus::Disconnected`] — so a
//! transient I/O failure never registers a phantom transition. In threaded mode
//! status is sampled at the poll thread's cadence rather than per graph cycle,
//! and is carried in-band with the data so a `Connected` transition stays
//! ordered before the fragments that followed it.
//!
//! # Deviations from classic
//!
//! Every classic *capability* (both backends, both polling modes, the typed
//! parser with `FragmentHeader` access, the `fragment_limit` cap, the
//! `Spin`→`Threaded` downgrade, both status side-channels, the `ChannelUri`
//! builders, `ClaimBuffer`'s zero-copy commit/abort contract, and the
//! `TransportError` / `AeronStatus` types) is preserved. The surface differs in
//! these deliberate ways:
//!
//! 1. **The sources take a [`GraphBuilder`](crate::fluent::GraphBuilder) and a
//!    [`RunMode`](wingfoil_next::RunMode), and return [`Result`](anyhow::Result).**
//!    Every next source wires on the builder, and the run mode is needed to
//!    **reject `RunMode::HistoricalFrom` at wiring**: a live Aeron subscription
//!    has no historical timeline to replay, and the `Threaded` mode rides the
//!    channel layer, whose historical receiver would block-collect the
//!    never-closing poll thread and deadlock at `start` (register B2). Classic's
//!    spin subscriber silently ran against the fast-forwarded historical clock.
//!    The **publisher** keeps classic's behaviour exactly: its real-time check
//!    fires at graph `start()`, aborting the run.
//! 2. **The status side-channel is a plain stream, not a node type.** Classic
//!    exposed `AeronStatusStream`, a `MutableNode` the producer drove through
//!    `clear()`/`record()` and wired as an active downstream. next multiplexes
//!    status with data over one internal envelope and **splits** it into the
//!    `(data, status)` pair with `map_filter` — the same shape the
//!    [`zmq`](crate::adapters::zmq) subscriber uses. The observable behaviour
//!    (transition-only emission, derivation order, in-band ordering in threaded
//!    mode) is identical, but `AeronStatusStream` itself has no next twin; the
//!    status half is an ordinary `Stream<Burst<AeronStatus>>`. Notably this also
//!    makes the *spin* mode carry status in-band, where classic used a shared
//!    `Rc<RefCell<..>>`.
//! 3. **The sink is an extension trait returning `Stream<()>`.** Classic's
//!    `AeronPub` returned `Rc<dyn Node>`; [`AeronSinkOps`] returns the sink
//!    stream, per the sink-as-trait convention shared with
//!    [`lines`](crate::adapters::lines) / [`csv`](crate::adapters::csv) /
//!    [`kafka`](crate::adapters::kafka).
//! 4. **The mock backends are public.** Classic gated `MockSubscriber` /
//!    `MockPublisher` behind `#[cfg(test)]` inside the crate; next's adapter
//!    tests live in `tests/` and compile against the public library, so they are
//!    public, always-compiled test support (tiny and dependency-free).
//! 5. **The plain `aeron_sub_fragment` never derives status.** Classic's spin
//!    node held an `Option<Rc<RefCell<AeronStatusStream>>>` and skipped the
//!    derivation when `None`; next passes the same choice as a `track_status`
//!    flag. Same behaviour, no allocation.
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
//! Constructing a subscription/publication handle **requires a live media
//! driver** — there is no offline construction path, which is why the adapter's
//! own tests drive the [`MockSubscriber`](transport::MockSubscriber) /
//! [`MockPublisher`](transport::MockPublisher) backends.
//!
//! The `aeron` (rusteron) backend builds the Aeron C library from source, so it
//! needs `cmake >= 3.30`, `clang`, `uuid-dev` and `libbsd-dev` on the build
//! machine; the pure-Rust `aeron-rs` backend needs none of that.
//!
//! # Subscribing (spin mode — rusteron backend)
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil_next::{RunFor, RunMode};
//! use wingfoil_next::adapters::aeron::{
//!     AeronHandle, AeronSubOptions, FragmentBuffer, TransportError, aeron_sub_fragment,
//! };
//! use wingfoil_next::prelude::*;
//!
//! let handle = AeronHandle::connect()?;
//! let sub = handle.subscription("aeron:ipc", 1001, Duration::from_secs(5))?;
//!
//! let g = GraphBuilder::new();
//! let _sink = aeron_sub_fragment(
//!     &g,
//!     RunMode::RealTime,
//!     sub,
//!     |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
//!         Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
//!     },
//!     AeronSubOptions::default(),
//! )?
//! .map(|burst: &Burst<i64>| burst.iter().sum::<i64>())
//! .print();
//! g.build().run(RunMode::RealTime, RunFor::Forever)?;
//! ```
//!
//! # Publishing
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil_next::{RunFor, RunMode};
//! use wingfoil_next::adapters::aeron::{AeronHandle, AeronSinkOps};
//! use wingfoil_next::prelude::*;
//!
//! let handle = AeronHandle::connect()?;
//! let publication = handle.publication("aeron:ipc", 1002, Duration::from_secs(5))?;
//!
//! let g = GraphBuilder::new();
//! let _sink = g
//!     .ticker(Duration::from_millis(100))
//!     .count()
//!     .map(|n: &u64| burst![*n])
//!     .aeron_pub(publication, |v: &u64| v.to_le_bytes().to_vec());
//! g.build().run(RunMode::RealTime, RunFor::Forever)?;
//! ```

pub mod buffer;
pub mod error;
pub mod status;
pub mod transport;

mod channel;
mod read;
mod write;

#[cfg(feature = "aeron")]
pub mod rusteron_backend;

#[cfg(feature = "aeron-rs")]
pub mod aeron_rs_backend;

pub use buffer::{ClaimBuffer, FragmentBuffer, FragmentHeader};
pub use channel::ChannelUri;
pub use error::TransportError;
pub use status::AeronStatus;
pub use transport::{AeronPublisherBackend, AeronSubscriberBackend};
pub use write::AeronSinkOps;

#[cfg(feature = "aeron")]
pub use rusteron_backend::AeronHandle;

#[cfg(feature = "aeron-rs")]
pub use aeron_rs_backend::AeronRsHandle;

use anyhow::Result;
use wingfoil_next::RunMode;

use crate::Burst;
use crate::fluent::{GraphBuilder, Stream};

// ---------------------------------------------------------------------------
// AeronMode / options
// ---------------------------------------------------------------------------

/// Selects how the subscriber polls Aeron.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeronMode {
    /// Poll Aeron inside the graph cycle on the graph thread (primary pattern).
    ///
    /// Zero thread-crossing latency; consumes one CPU core. Ticks only when
    /// fragments arrive so downstream nodes stay reactive.
    Spin,

    /// Poll Aeron on a dedicated background thread (secondary pattern).
    ///
    /// Adds one channel-hop of latency but frees the graph thread for other
    /// work. Only [`RunMode::RealTime`] is supported.
    Threaded,
}

/// Default cap on fragments processed per `poll()` cycle.
///
/// `256` is the Aeron sample harness convention
/// (`aeron.sample.frameCountLimit=256`, per the Aeron Java Programming Guide).
/// The value is a **cap, not a target**: `poll()` returns control after at most
/// this many fragments OR when no more are immediately available, whichever
/// comes first.
///
/// **Trade-off:**
/// - Lower values (e.g. `32`) reduce per-cycle latency tail but add
///   per-fragment loop overhead.
/// - Higher values (e.g. `1024`) amortise loop overhead but lengthen the
///   worst-case `poll()` cycle.
pub const DEFAULT_FRAGMENT_LIMIT: usize = 256;

/// Subscriber options bag passed to the source factories.
///
/// - `mode` selects [`AeronMode::Spin`] or [`AeronMode::Threaded`].
/// - `fragment_limit` flows through to the backend's per-`poll()` cap.
///
/// Construct via `AeronSubOptions::default()` (yields `mode: Spin`,
/// `fragment_limit: 256`) or with explicit fields.
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

/// Resolve the polling mode actually used for `subscriber`.
///
/// A backend that cannot be polled on the graph thread (see
/// [`AeronSubscriberBackend::supports_graph_thread_poll`]) downgrades a
/// requested [`AeronMode::Spin`] to [`AeronMode::Threaded`], so its lock never
/// runs inside the graph cycle. This keeps the `aeron-rs` backend from violating
/// the no-locks-on-the-graph-path invariant by construction — `Spin` is simply
/// unreachable for it. Lock-free backends (rusteron, the mocks) pass the
/// requested mode through unchanged.
fn effective_mode<B: AeronSubscriberBackend>(requested: AeronMode, subscriber: &B) -> AeronMode {
    if requested == AeronMode::Spin && !subscriber.supports_graph_thread_poll() {
        log::warn!(
            "aeron sub: backend cannot be polled on the graph thread (it locks on poll); \
             downgrading AeronMode::Spin to Threaded to honour the no-locks-in-cycle invariant"
        );
        AeronMode::Threaded
    } else {
        requested
    }
}

/// The shared wiring-time live-source guard (register B2).
fn reject_historical(run_mode: RunMode, who: &str) -> Result<()> {
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "{who}: RunMode::HistoricalFrom is unsupported — an Aeron subscription is \
             a live, unbounded source with no historical timeline to replay; run \
             {who} under RunMode::RealTime"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Source factories
// ---------------------------------------------------------------------------

/// Poll an Aeron subscription and emit each cycle's decoded fragments as one
/// [`Burst`].
///
/// The typed parser `FnMut(&FragmentBuffer<'_>) -> Result<Option<T>,
/// TransportError>` has access to the per-fragment header (`position`,
/// `session_id`, `stream_id`) and may signal recoverable errors:
/// `Ok(Some(v))` pushes `v` onto the burst, `Ok(None)` silently skips the
/// fragment, and `Err(_)` is logged at WARN and skipped — a malformed fragment
/// never aborts the cycle.
///
/// # The `Send` parser bound
///
/// The factory is mode-polymorphic (it chooses spin or threaded at runtime from
/// `opts.mode`); the threaded variant moves the parser onto a background thread,
/// so the parser bound must be `Send` even though the spin variant alone would
/// not require it.
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`] — see deviation 1 in the module docs. A poll
/// error aborts the run: on the firing cycle in [`AeronMode::Spin`], or via the
/// channel's error path in [`AeronMode::Threaded`].
pub fn aeron_sub_fragment<T, F, B>(
    g: &GraphBuilder,
    run_mode: RunMode,
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + Send + 'static,
    F: FnMut(&buffer::FragmentBuffer<'_>) -> Result<Option<T>, error::TransportError>
        + Send
        + 'static,
    B: AeronSubscriberBackend,
{
    reject_historical(run_mode, "aeron_sub_fragment")?;
    let events = read::event_source(g, subscriber, parser, opts, false);
    let (data, _status) = read::split_events(&events);
    Ok(data)
}

/// [`aeron_sub_fragment`], paired with a reactive status stream.
///
/// Parallel-additive sibling: returns `(data, status)`, where `data` is
/// identical in shape to [`aeron_sub_fragment`]'s output and `status` emits a
/// `Burst<AeronStatus>` **only on transition cycles** (no re-emission in steady
/// state). See the module docs for the derivation order and the threaded-mode
/// sampling cadence.
///
/// # Errors
///
/// As [`aeron_sub_fragment`].
pub fn aeron_sub_fragment_with_status<T, F, B>(
    g: &GraphBuilder,
    run_mode: RunMode,
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
) -> Result<(Stream<Burst<T>>, Stream<Burst<AeronStatus>>)>
where
    T: Clone + Default + Send + 'static,
    F: FnMut(&buffer::FragmentBuffer<'_>) -> Result<Option<T>, error::TransportError>
        + Send
        + 'static,
    B: AeronSubscriberBackend,
{
    reject_historical(run_mode, "aeron_sub_fragment_with_status")?;
    let events = read::event_source(g, subscriber, parser, opts, true);
    Ok(read::split_events(&events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_fragment_limit_is_256() {
        assert_eq!(DEFAULT_FRAGMENT_LIMIT, 256);
        let opts = AeronSubOptions::default();
        assert_eq!(opts.fragment_limit, 256);
        assert!(matches!(opts.mode, AeronMode::Spin));
    }

    /// Minimal subscriber that reports it cannot be polled on the graph thread,
    /// to exercise the `Spin` → `Threaded` downgrade in [`effective_mode`].
    struct NoGraphThreadPollSubscriber;

    impl AeronSubscriberBackend for NoGraphThreadPollSubscriber {
        fn poll(&mut self, _handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
            Ok(0)
        }

        fn supports_graph_thread_poll(&self) -> bool {
            false
        }
    }

    #[test]
    fn non_graph_thread_backend_downgrades_spin_to_threaded() {
        let backend = NoGraphThreadPollSubscriber;
        assert_eq!(
            effective_mode(AeronMode::Spin, &backend),
            AeronMode::Threaded,
            "a backend that locks on poll must never run Spin on the graph thread"
        );
        assert_eq!(
            effective_mode(AeronMode::Threaded, &backend),
            AeronMode::Threaded
        );
    }

    #[test]
    fn lock_free_backend_keeps_spin() {
        // `MockSubscriber` inherits `supports_graph_thread_poll() == true`.
        let backend = transport::MockSubscriber::from_messages(vec![]);
        assert_eq!(effective_mode(AeronMode::Spin, &backend), AeronMode::Spin);
    }
}
