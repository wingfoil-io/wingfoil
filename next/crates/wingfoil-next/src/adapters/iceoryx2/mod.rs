//! iceoryx2 adapter — zero-copy inter-process (and intra-process)
//! publish/subscribe over shared memory. It ports the classic
//! `wingfoil::adapters::iceoryx2` module onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `iceoryx2` feature. Bring in what you need explicitly:
//!
//! - **Sources** — the free builder functions [`iceoryx2_sub`] /
//!   [`iceoryx2_sub_with`] / [`iceoryx2_sub_opts`] (typed payloads) and
//!   [`iceoryx2_sub_slice`] / [`iceoryx2_sub_slice_opts`] (`[u8]` slices) on a
//!   [`GraphBuilder`](crate::fluent::GraphBuilder): subscribe to a service and
//!   emit each cycle's samples as one [`Burst`](crate::Burst).
//! - **Sinks** — the [`Iceoryx2SinkOps`] extension trait on
//!   `Stream<Burst<T>>` and [`Iceoryx2SliceSinkOps`] on `Stream<Burst<Vec<u8>>>`,
//!   enabled with
//!   `use wingfoil_next::adapters::iceoryx2::Iceoryx2SinkOps;`.
//!
//! Like the classic adapter (and unlike the networked async adapters) it is
//! synchronous and poll-based, so the `iceoryx2` feature deliberately does
//! **not** pull in `async`/tokio.
//!
//! # Source semantics — three polling modes
//!
//! The subscriber offers the classic [`Iceoryx2Mode`] trade-off, selected per
//! subscriber and returning the same `Stream<Burst<T>>` either way:
//!
//! - [`Iceoryx2Mode::Spin`] (default) — a busy-spin
//!   [`custom_node`](crate::fluent::GraphBuilder::custom_node) polling the
//!   subscriber port on the graph thread every cycle. Lowest latency (~1–5 µs),
//!   highest CPU (the kernel never parks while it exists).
//! - [`Iceoryx2Mode::Threaded`] — a background OS thread polls with a 10 µs
//!   yield and feeds the [`channel`](crate::fluent::SourceOps::channel) layer.
//!   Lower CPU, one channel-hop of latency.
//! - [`Iceoryx2Mode::Signaled`] — event-driven, blocking on a `WaitSet`
//!   attached to the service's `<name>.signal` Event service (which the
//!   publisher notifies). Lowest CPU, highest latency.
//!
//! Samples received between graph cycles group into one [`Burst`](crate::Burst)
//! — for `Spin` because the cycle drains the port into a burst, for the other
//! two because the channel layer groups same-instant values.
//!
//! All three are **live, realtime-only** sources: a
//! [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom) run is
//! rejected at wiring (see the deviations below).
//!
//! # Sink
//!
//! The publisher loans a shared-memory sample per value in the burst, writes the
//! payload into it, and sends it; after a non-empty burst it notifies the
//! service's Event listener so a `Signaled` subscriber wakes. A loan/send
//! failure aborts the run with context. Connections are refreshed every tenth
//! cycle (`update_connections`), matching classic.
//!
//! # Zero-copy requirements
//!
//! Typed payloads must implement [`ZeroCopySend`](iceoryx2::prelude::ZeroCopySend),
//! be `#[repr(C)]` and self-contained (no heap allocations, no pointers to
//! external data). Use the slice API ([`iceoryx2_sub_slice`] /
//! [`Iceoryx2SliceSinkOps::iceoryx2_pub_slice`]) to carry variable-length bytes,
//! or [`FixedBytes`] to carry them through the typed API.
//!
//! # Service contracts
//!
//! `history_size` and `subscriber_max_buffer_size` are part of the iceoryx2
//! publish/subscribe *service configuration*: every participant opening or
//! creating the same service must agree. A mismatch surfaces as
//! [`Iceoryx2Error::ServiceConfigMismatch`] (or
//! [`Iceoryx2Error::ServiceOpenFailed`], depending on what iceoryx2 reports)
//! carrying the service name, variant, and both sizes —
//! [`Iceoryx2ServiceContract`] / [`Iceoryx2SliceContract`] make the derived
//! values inspectable at wiring.
//!
//! # Deviations from classic
//!
//! Every classic *capability* (typed and slice payloads, all three polling
//! modes, the `Ipc`/`Local` service variants, the option/`_with`/`_opts`
//! constructor family, the service contracts, `FixedBytes`, and the typed
//! [`Iceoryx2Error`]) is preserved. The surface differs in these deliberate ways:
//!
//! 1. **The sources take a [`GraphBuilder`](crate::fluent::GraphBuilder) and a
//!    [`RunMode`](wingfoil::RunMode), and return
//!    [`Result`](anyhow::Result).** Every next source wires on the builder, and
//!    the run mode is needed to **reject `RunMode::HistoricalFrom` at wiring**: a
//!    live shared-memory subscription has no historical timeline to replay, and
//!    the `Threaded`/`Signaled` modes ride the channel layer, whose historical
//!    receiver would block-collect the never-closing producer and deadlock at
//!    `start` (register B2). Classic silently ran the poll loop against the
//!    fast-forwarded historical clock.
//! 2. **The sinks are extension traits.** Classic exposed free `iceoryx2_pub*`
//!    functions taking the upstream; next folds them into
//!    [`Iceoryx2SinkOps`] / [`Iceoryx2SliceSinkOps`], per the sink-as-trait
//!    convention shared with [`lines`](crate::adapters::lines) /
//!    [`csv`](crate::adapters::csv) / [`kafka`](crate::adapters::kafka), and they
//!    return the sink `Stream<()>` rather than an `Rc<dyn Node>`.
//! 3. **The sink does *not* reject or no-op under historical replay** — classic
//!    parity, deliberately kept. Unlike [`zmq_pub`](crate::adapters::zmq) (which
//!    errors) and the telemetry exporters (which no-op), classic's iceoryx2
//!    publisher publishes under either run mode, and a backtest that pipes its
//!    output into shared memory is a legitimate use.
//! 4. **Ports are created at graph `start()`**, as in classic, but wiring itself
//!    is now pure: the service name, variant, and contract are only validated
//!    once the run begins, so a bad service name or a contract mismatch aborts
//!    the *run* with node context rather than graph construction (register
//!    A1/A4). This matches classic, whose `start` did the same.
//!
//! # Setup
//!
//! iceoryx2 needs writable shared memory; on Linux this is `/dev/shm`, normally
//! available out of the box. `Iceoryx2ServiceVariant::Local` needs nothing at
//! all — it is in-process, heap-backed, and is what the adapter's own tests use.
//! Stale shared-memory segments left by a crashed process may need manual
//! cleanup under `/dev/shm/`.

use iceoryx2::config::Config;
use iceoryx2::node::Node;
use iceoryx2::prelude::ZeroCopySend;
use iceoryx2::service::{ipc, local};

mod read;
mod write;

pub use read::*;
pub use write::*;

/// A live iceoryx2 node handle, held for the lifetime of a port so the
/// underlying service stays open.
pub(crate) enum Iceoryx2NodeHandle {
    #[allow(dead_code)]
    Ipc(Node<ipc::Service>),
    #[allow(dead_code)]
    Local(Node<local::Service>),
}

/// Built-in iceoryx2 defaults, cached for the process lifetime.
///
/// Passed to every `NodeBuilder::new().config(...)` so iceoryx2's global
/// singleton is never lazily loaded from disk — that lookup emits a
/// "No config file was loaded" warning when no `config/iceoryx2.toml`
/// is found, which we don't want surfaced to users of the adapter.
pub(crate) fn iceoryx2_default_config() -> &'static Config {
    use std::sync::OnceLock;
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(Config::default)
}

/// Default number of past samples a late-joining subscriber receives.
pub const ICEORYX2_DEFAULT_HISTORY_SIZE: usize = 5;
/// Default subscriber receive-buffer size (the floor a contract derives).
pub const ICEORYX2_DEFAULT_SUBSCRIBER_MAX_BUFFER_SIZE: usize = 16;
/// Default maximum slice length for a `[u8]` publisher's loans.
pub const ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN: usize = 128 * 1024;

/// Service-level publish/subscribe configuration for an iceoryx2 service.
///
/// This is part of the service contract: all participants opening/creating the
/// same service must use compatible settings.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iceoryx2ServiceContract {
    /// How many past samples a late-joining subscriber receives.
    pub history_size: usize,
    /// The subscriber's receive-buffer size, derived from `history_size`.
    pub subscriber_max_buffer_size: usize,
}

impl Iceoryx2ServiceContract {
    /// Derive a contract from `history_size`, flooring the subscriber buffer at
    /// [`ICEORYX2_DEFAULT_SUBSCRIBER_MAX_BUFFER_SIZE`].
    #[must_use]
    pub fn new(history_size: usize) -> Self {
        Self {
            history_size,
            subscriber_max_buffer_size: history_size
                .max(ICEORYX2_DEFAULT_SUBSCRIBER_MAX_BUFFER_SIZE),
        }
    }
}

impl Default for Iceoryx2ServiceContract {
    fn default() -> Self {
        Self::new(ICEORYX2_DEFAULT_HISTORY_SIZE)
    }
}

/// Service-level configuration for slice publishers (`[u8]`), which also require
/// a maximum slice length.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iceoryx2SliceContract {
    /// The shared publish/subscribe contract.
    pub pubsub: Iceoryx2ServiceContract,
    /// The largest slice the publisher can loan.
    pub initial_max_slice_len: usize,
}

impl Iceoryx2SliceContract {
    /// Derive a slice contract from `history_size` and `initial_max_slice_len`.
    #[must_use]
    pub fn new(history_size: usize, initial_max_slice_len: usize) -> Self {
        Self {
            pubsub: Iceoryx2ServiceContract::new(history_size),
            initial_max_slice_len,
        }
    }
}

impl Default for Iceoryx2SliceContract {
    fn default() -> Self {
        Self::new(
            ICEORYX2_DEFAULT_HISTORY_SIZE,
            ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN,
        )
    }
}

/// Which iceoryx2 service variant to use.
///
/// - [`Iceoryx2ServiceVariant::Ipc`]: inter-process communication (shared memory)
/// - [`Iceoryx2ServiceVariant::Local`]: intra-process communication (heap)
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum Iceoryx2ServiceVariant {
    /// Inter-process communication over shared memory.
    #[default]
    Ipc,
    /// Intra-process communication over the heap.
    Local,
}

/// Polling mode for the iceoryx2 subscriber.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum Iceoryx2Mode {
    /// Polls directly inside the graph cycle.
    /// Lowest latency, highest CPU usage (on the graph thread).
    #[default]
    Spin,
    /// Polls in a dedicated background thread and delivers via the channel layer.
    /// Higher latency (one channel-hop), lower CPU usage (uses a 10 µs yield).
    Threaded,
    /// Event-driven `WaitSet` (true blocking).
    /// Requires the publisher to signal on the matching Event service.
    Signaled,
}

/// Configuration options for an iceoryx2 subscriber.
#[derive(Debug, Clone)]
pub struct Iceoryx2SubOpts {
    /// `Ipc` (shared memory) or `Local` (in-process).
    pub variant: Iceoryx2ServiceVariant,
    /// Which polling strategy the subscriber uses.
    pub mode: Iceoryx2Mode,
    /// How many past samples a late joiner receives (part of the contract).
    pub history_size: usize,
}

impl Default for Iceoryx2SubOpts {
    fn default() -> Self {
        Self {
            variant: Iceoryx2ServiceVariant::default(),
            mode: Iceoryx2Mode::default(),
            history_size: ICEORYX2_DEFAULT_HISTORY_SIZE,
        }
    }
}

impl Iceoryx2SubOpts {
    /// The service contract these options imply.
    #[must_use]
    pub fn contract(&self) -> Iceoryx2ServiceContract {
        Iceoryx2ServiceContract::new(self.history_size)
    }
}

/// Configuration options for an iceoryx2 publisher (typed payloads).
///
/// Note: `history_size` is part of the iceoryx2 publish/subscribe *service
/// configuration*. All participants opening/creating the same service must use
/// compatible settings.
#[derive(Debug, Clone)]
pub struct Iceoryx2PubOpts {
    /// `Ipc` (shared memory) or `Local` (in-process).
    pub variant: Iceoryx2ServiceVariant,
    /// How many past samples a late joiner receives (part of the contract).
    pub history_size: usize,
}

impl Default for Iceoryx2PubOpts {
    fn default() -> Self {
        Self {
            variant: Iceoryx2ServiceVariant::default(),
            history_size: ICEORYX2_DEFAULT_HISTORY_SIZE,
        }
    }
}

impl Iceoryx2PubOpts {
    /// The service contract these options imply.
    #[must_use]
    pub fn contract(&self) -> Iceoryx2ServiceContract {
        Iceoryx2ServiceContract::new(self.history_size)
    }
}

/// Configuration options for an iceoryx2 slice publisher (`[u8]` payloads).
#[derive(Debug, Clone)]
pub struct Iceoryx2PubSliceOpts {
    /// `Ipc` (shared memory) or `Local` (in-process).
    pub variant: Iceoryx2ServiceVariant,
    /// How many past samples a late joiner receives (part of the contract).
    pub history_size: usize,
    /// The largest slice the publisher can loan.
    pub initial_max_slice_len: usize,
}

impl Default for Iceoryx2PubSliceOpts {
    fn default() -> Self {
        Self {
            variant: Iceoryx2ServiceVariant::default(),
            history_size: ICEORYX2_DEFAULT_HISTORY_SIZE,
            initial_max_slice_len: ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN,
        }
    }
}

impl Iceoryx2PubSliceOpts {
    /// The slice service contract these options imply.
    #[must_use]
    pub fn contract(&self) -> Iceoryx2SliceContract {
        Iceoryx2SliceContract::new(self.history_size, self.initial_max_slice_len)
    }
}

/// A fixed-size byte buffer that implements `ZeroCopySend`.
///
/// Carries variable-length bytes through the *typed* API (the slice API is the
/// alternative). Longer input is truncated to `N`.
#[repr(C)]
#[derive(Debug, Clone, Copy, ZeroCopySend)]
pub struct FixedBytes<const N: usize> {
    /// How many of `data`'s bytes are meaningful.
    pub len: usize,
    /// The fixed-capacity payload buffer.
    pub data: [u8; N],
}

impl<const N: usize> Default for FixedBytes<N> {
    fn default() -> Self {
        Self {
            len: 0,
            data: [0; N],
        }
    }
}

impl<const N: usize> FixedBytes<N> {
    /// Copy up to `N` bytes out of `bytes`, truncating anything longer.
    pub fn new(bytes: &[u8]) -> Self {
        let mut data = [0; N];
        let len = bytes.len().min(N);
        data[..len].copy_from_slice(&bytes[..len]);
        Self { len, data }
    }

    /// The meaningful prefix of the buffer.
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// Errors that can occur in the iceoryx2 adapter.
#[derive(Debug, thiserror::Error)]
pub enum Iceoryx2Error {
    /// The iceoryx2 node could not be created.
    #[error("Failed to create node: {0}")]
    NodeCreationFailed(String),
    /// The publish/subscribe service could not be opened or created.
    #[error(
        "Failed to open/create service: {error} (service_name={service_name:?}, variant={variant:?}, history_size={history_size}, subscriber_max_buffer_size={subscriber_max_buffer_size})"
    )]
    ServiceOpenFailed {
        /// The underlying iceoryx2 error, rendered.
        error: String,
        /// The service the adapter was opening.
        service_name: String,
        /// `Ipc` or `Local`.
        variant: Iceoryx2ServiceVariant,
        /// The contract's history size.
        history_size: usize,
        /// The contract's derived subscriber buffer size.
        subscriber_max_buffer_size: usize,
    },
    /// The service exists but was created with an incompatible contract.
    #[error(
        "Service config mismatch: {error} (service_name={service_name:?}, variant={variant:?}, history_size={history_size}, subscriber_max_buffer_size={subscriber_max_buffer_size})"
    )]
    ServiceConfigMismatch {
        /// The underlying iceoryx2 error, rendered.
        error: String,
        /// The service the adapter was opening.
        service_name: String,
        /// `Ipc` or `Local`.
        variant: Iceoryx2ServiceVariant,
        /// The contract's history size.
        history_size: usize,
        /// The contract's derived subscriber buffer size.
        subscriber_max_buffer_size: usize,
    },
    /// A publisher, subscriber, notifier or listener port could not be created.
    #[error("Failed to create port: {0}")]
    PortCreationFailed(String),
    /// A shared-memory operation failed.
    #[error("Shared memory error: {0}")]
    SharedMemoryError(String),
    /// A loan, write or send failed.
    #[error("Data transmission error: {0}")]
    TransmissionError(String),
    /// Anything else, carried as an `anyhow::Error`.
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

/// The adapter's result alias.
pub type Iceoryx2Result<T> = Result<T, Iceoryx2Error>;

impl From<iceoryx2::service::service_name::ServiceNameError> for Iceoryx2Error {
    fn from(e: iceoryx2::service::service_name::ServiceNameError) -> Self {
        Self::Other(anyhow::anyhow!(e.to_string()))
    }
}

impl From<iceoryx2::waitset::WaitSetCreateError> for Iceoryx2Error {
    fn from(e: iceoryx2::waitset::WaitSetCreateError) -> Self {
        Self::Other(anyhow::anyhow!(e.to_string()))
    }
}

impl From<iceoryx2::waitset::WaitSetRunError> for Iceoryx2Error {
    fn from(e: iceoryx2::waitset::WaitSetRunError) -> Self {
        Self::Other(anyhow::anyhow!(e.to_string()))
    }
}

/// Classify an `open_or_create` failure as a contract mismatch or a plain open
/// failure, attaching the service name, variant and contract sizes either way.
///
/// iceoryx2 reports both through the same error type, so the message is
/// inspected — the classic adapter's heuristic, ported verbatim.
pub(crate) fn service_open_err_with_context(
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
    contract: Iceoryx2ServiceContract,
    err: impl std::fmt::Display,
) -> Iceoryx2Error {
    let error = err.to_string();
    let looks_like_mismatch = {
        let e = error.to_lowercase();
        e.contains("incompatible") || e.contains("mismatch") || e.contains("config")
    };

    if looks_like_mismatch {
        Iceoryx2Error::ServiceConfigMismatch {
            error,
            service_name: service_name.to_string(),
            variant,
            history_size: contract.history_size,
            subscriber_max_buffer_size: contract.subscriber_max_buffer_size,
        }
    } else {
        Iceoryx2Error::ServiceOpenFailed {
            error,
            service_name: service_name.to_string(),
            variant,
            history_size: contract.history_size,
            subscriber_max_buffer_size: contract.subscriber_max_buffer_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_bytes_edge_cases() {
        // Truncation
        let fb = FixedBytes::<5>::new(b"hello world");
        assert_eq!(fb.len, 5);
        assert_eq!(fb.as_slice(), b"hello");

        // Zero-length
        let empty = FixedBytes::<10>::new(b"");
        assert_eq!(empty.len, 0);
        assert_eq!(empty.as_slice(), b"");

        // Exactly full
        let full = FixedBytes::<3>::new(b"abc");
        assert_eq!(full.len, 3);
        assert_eq!(full.as_slice(), b"abc");
    }

    #[test]
    fn sub_opts_defaults() {
        let opts = Iceoryx2SubOpts::default();
        assert_eq!(opts.variant, Iceoryx2ServiceVariant::Ipc);
        assert_eq!(opts.mode, Iceoryx2Mode::Spin);
        assert!(opts.history_size > 0);
    }

    #[test]
    fn service_contract_derives_buffer_size() {
        let c = Iceoryx2ServiceContract::new(1);
        assert_eq!(c.history_size, 1);
        assert!(c.subscriber_max_buffer_size >= ICEORYX2_DEFAULT_SUBSCRIBER_MAX_BUFFER_SIZE);

        let c2 = Iceoryx2ServiceContract::new(64);
        assert_eq!(c2.history_size, 64);
        assert_eq!(c2.subscriber_max_buffer_size, 64);
    }

    #[test]
    fn opts_expose_service_contract() {
        let sub_opts = Iceoryx2SubOpts {
            variant: Iceoryx2ServiceVariant::Local,
            mode: Iceoryx2Mode::Spin,
            history_size: 7,
        };
        assert_eq!(sub_opts.contract(), Iceoryx2ServiceContract::new(7));

        let pub_opts = Iceoryx2PubOpts {
            variant: Iceoryx2ServiceVariant::Ipc,
            history_size: 9,
        };
        assert_eq!(pub_opts.contract(), Iceoryx2ServiceContract::new(9));

        let slice_opts = Iceoryx2PubSliceOpts {
            variant: Iceoryx2ServiceVariant::Ipc,
            history_size: 11,
            initial_max_slice_len: 256 * 1024,
        };
        assert_eq!(
            slice_opts.contract(),
            Iceoryx2SliceContract::new(11, 256 * 1024)
        );
    }

    #[test]
    fn open_error_classified_as_mismatch_when_message_hints_at_it() {
        let contract = Iceoryx2ServiceContract::new(7);
        let mismatch = service_open_err_with_context(
            "svc",
            Iceoryx2ServiceVariant::Local,
            contract,
            "IncompatibleSubscriberBufferSize",
        );
        assert!(matches!(
            mismatch,
            Iceoryx2Error::ServiceConfigMismatch { .. }
        ));

        let plain = service_open_err_with_context(
            "svc",
            Iceoryx2ServiceVariant::Local,
            contract,
            "InsufficientPermissions",
        );
        assert!(matches!(plain, Iceoryx2Error::ServiceOpenFailed { .. }));
    }
}
