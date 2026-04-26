//! iceoryx2 adapter — zero-copy inter-process communication (IPC)
//!
//! Provides two graph nodes:
//!
//! - [`iceoryx2_sub`] — subscribes to an iceoryx2 service and produces a stream
//! - [`iceoryx2_pub`] — publishes a stream to an iceoryx2 service
//!
//! # Setup
//!
//! iceoryx2 requires shared memory to be available. On Linux, this is typically
//! pre-configured. The service uses IPC (inter-process) mode by default.
//!
//! # Zero-Copy Requirements
//!
//! Payload types must implement [`ZeroCopySend`] and be `#[repr(C)]` and self-contained
//! (no heap allocations, no pointers to external data).

use iceoryx2::node::Node;
use iceoryx2::prelude::ZeroCopySend;
use iceoryx2::service::{ipc, local};

pub(crate) enum Iceoryx2NodeHandle {
    #[allow(dead_code)]
    Ipc(Node<ipc::Service>),
    #[allow(dead_code)]
    Local(Node<local::Service>),
}

pub const ICEORYX2_DEFAULT_HISTORY_SIZE: usize = 5;
pub const ICEORYX2_DEFAULT_SUBSCRIBER_MAX_BUFFER_SIZE: usize = 16;
pub const ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN: usize = 128 * 1024;

/// Service-level publish/subscribe configuration for an iceoryx2 service.
///
/// This is part of the service contract: all participants opening/creating the same service
/// must use compatible settings.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iceoryx2ServiceContract {
    pub history_size: usize,
    pub subscriber_max_buffer_size: usize,
}

impl Iceoryx2ServiceContract {
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

/// Service-level configuration for slice publishers (`[u8]`) which also require a max slice len.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iceoryx2SliceContract {
    pub pubsub: Iceoryx2ServiceContract,
    pub initial_max_slice_len: usize,
}

impl Iceoryx2SliceContract {
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
    #[default]
    Ipc,
    Local,
}

/// Polling mode for the iceoryx2 subscriber.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum Iceoryx2Mode {
    /// Polls directly inside the graph `cycle()` loop.
    /// Lowest latency, highest CPU usage (on graph thread).
    #[default]
    Spin,
    /// Polls in a dedicated background thread and delivers via channel.
    /// Higher latency (one channel-hop), lower CPU usage (uses 10µs yield).
    Threaded,
    /// Event-driven WaitSet (true blocking).
    /// Requires publisher signal on matching Event service.
    Signaled,
}

/// Configuration options for an iceoryx2 subscriber.
#[derive(Debug, Clone)]
pub struct Iceoryx2SubOpts {
    pub variant: Iceoryx2ServiceVariant,
    pub mode: Iceoryx2Mode,
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
    #[must_use]
    pub fn contract(&self) -> Iceoryx2ServiceContract {
        Iceoryx2ServiceContract::new(self.history_size)
    }
}

/// Configuration options for an iceoryx2 publisher (typed payloads).
///
/// Note: `history_size` is part of the iceoryx2 publish/subscribe *service configuration*.
/// All participants opening/creating the same service must use compatible settings.
#[derive(Debug, Clone)]
pub struct Iceoryx2PubOpts {
    pub variant: Iceoryx2ServiceVariant,
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
    #[must_use]
    pub fn contract(&self) -> Iceoryx2ServiceContract {
        Iceoryx2ServiceContract::new(self.history_size)
    }
}

/// Configuration options for an iceoryx2 slice publisher (`[u8]` payloads).
#[derive(Debug, Clone)]
pub struct Iceoryx2PubSliceOpts {
    pub variant: Iceoryx2ServiceVariant,
    pub history_size: usize,
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
    #[must_use]
    pub fn contract(&self) -> Iceoryx2SliceContract {
        Iceoryx2SliceContract::new(self.history_size, self.initial_max_slice_len)
    }
}

/// A fixed-size byte buffer that implements `ZeroCopySend`.
/// Used for generic data transfer (e.g. in Python bindings).
#[repr(C)]
#[derive(Debug, Clone, Copy, ZeroCopySend)]
pub struct FixedBytes<const N: usize> {
    pub len: usize,
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
    pub fn new(bytes: &[u8]) -> Self {
        let mut data = [0; N];
        let len = bytes.len().min(N);
        data[..len].copy_from_slice(&bytes[..len]);
        Self { len, data }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// Errors that can occur in the iceoryx2 adapter.
#[derive(Debug, thiserror::Error)]
pub enum Iceoryx2Error {
    #[error("Failed to create node: {0}")]
    NodeCreationFailed(String),
    #[error(
        "Failed to open/create service: {error} (service_name={service_name:?}, variant={variant:?}, history_size={history_size}, subscriber_max_buffer_size={subscriber_max_buffer_size})"
    )]
    ServiceOpenFailed {
        error: String,
        service_name: String,
        variant: Iceoryx2ServiceVariant,
        history_size: usize,
        subscriber_max_buffer_size: usize,
    },
    #[error(
        "Service config mismatch: {error} (service_name={service_name:?}, variant={variant:?}, history_size={history_size}, subscriber_max_buffer_size={subscriber_max_buffer_size})"
    )]
    ServiceConfigMismatch {
        error: String,
        service_name: String,
        variant: Iceoryx2ServiceVariant,
        history_size: usize,
        subscriber_max_buffer_size: usize,
    },
    #[error("Failed to create port: {0}")]
    PortCreationFailed(String),
    #[error("Shared memory error: {0}")]
    SharedMemoryError(String),
    #[error("Data transmission error: {0}")]
    TransmissionError(String),
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

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

/// Check if RouDi daemon is available for IPC services.
/// Returns an error with helpful message if RouDi is not running.
pub fn check_roudi_availability() -> Iceoryx2Result<()> {
    use std::process::Command;

    // Check if iox-roudi process is running using ps + grep filter
    let ps_output = Command::new("sh")
        .args(&["-c", "ps aux | grep '[i]ox-roudi' | grep -v grep"])
        .output();

    match ps_output {
        Ok(output) if output.status.success() && !output.stdout.is_empty() => {
            // Process found - RouDi is running
            Ok(())
        }
        _ => {
            Err(Iceoryx2Error::Other(anyhow::anyhow!(
                "ERROR: RouDi daemon is not running!\n\n\
                 The iceoryx2 IPC adapter requires the RouDi daemon.\n\n\
                 To start RouDi, run:\n  \
                 iox-roudi &\n\n\
                 Alternative: Use Local variant for in-process testing without RouDi:\n  \
                 iceoryx2_sub_with::<T>(service_name, Iceoryx2ServiceVariant::Local)"
            )))
        }
    }
}

mod read;
mod write;

pub use read::*;
pub use write::*;

#[cfg(test)]
mod local_tests;

#[cfg(test)]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Burst, Graph, RunFor, RunMode};
    use iceoryx2::prelude::ZeroCopySend;

    #[test]
    fn test_burst_creation() {
        let mut burst: Burst<i32> = Burst::default();
        burst.push(42);
        assert_eq!(burst.len(), 1);
        assert_eq!(burst[0], 42);
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
    struct TestData {
        value: u64,
    }

    #[test]
    fn test_invalid_service_name_fails_fast() {
        // NOTE: Keep this test intentionally loose. We only care that invalid input fails fast,
        // not about exact error strings (which can change across iceoryx2 versions).
        let invalid_name = "";
        let sub = iceoryx2_sub::<TestData>(invalid_name);

        let graph_res = Graph::new(vec![sub.as_node()], RunMode::RealTime, RunFor::Cycles(1)).run();
        assert!(graph_res.is_err(), "expected invalid service name to error");
    }

    #[test]
    fn test_fixed_bytes_edge_cases() {
        // Truncation
        let input = b"hello world";
        let fb = FixedBytes::<5>::new(input);
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
    fn test_iceoryx2_sub_opts_defaults() {
        let opts = Iceoryx2SubOpts::default();
        assert_eq!(opts.variant, Iceoryx2ServiceVariant::Ipc);
        assert_eq!(opts.mode, Iceoryx2Mode::Spin);
        assert!(opts.history_size > 0);
    }

    #[test]
    fn test_service_contract_derives_buffer_size() {
        let c = Iceoryx2ServiceContract::new(1);
        assert_eq!(c.history_size, 1);
        assert!(c.subscriber_max_buffer_size >= ICEORYX2_DEFAULT_SUBSCRIBER_MAX_BUFFER_SIZE);

        let c2 = Iceoryx2ServiceContract::new(64);
        assert_eq!(c2.history_size, 64);
        assert_eq!(c2.subscriber_max_buffer_size, 64);
    }

    #[test]
    fn test_opts_expose_service_contract() {
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
}
