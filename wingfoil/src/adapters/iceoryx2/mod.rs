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

use iceoryx2::prelude::ZeroCopySend;

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
    /// Higher latency (one channel-hop), lower CPU usage (uses WaitSet).
    Threaded,
}

/// Configuration options for an iceoryx2 subscriber.
#[derive(Debug, Clone, Default)]
pub struct Iceoryx2SubOpts {
    pub variant: Iceoryx2ServiceVariant,
    pub mode: Iceoryx2Mode,
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

mod read;
mod write;

pub use read::*;
pub use write::*;

#[cfg(any(test, feature = "iceoryx2-integration-test"))]
pub mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::{NodeOperators, StreamOperators};
    use crate::{Burst, Graph, RunFor, RunMode, ticker};
    use iceoryx2::prelude::ZeroCopySend;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
    fn test_local_pubsub_smoke() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let service_name = format!("wingfoil/test/local/{}/{n}", std::process::id());

        let sub = iceoryx2_sub_with::<TestData>(&service_name, Iceoryx2ServiceVariant::Local);
        let collected = sub.collapse().collect();

        let upstream = ticker(Duration::from_millis(2)).produce(|| {
            let mut b: Burst<TestData> = Burst::default();
            b.push(TestData { value: 1 });
            b
        });
        let pub_node = iceoryx2_pub_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

        Graph::new(
            vec![pub_node, collected.clone().as_node()],
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(50)),
        )
        .run()
        .unwrap();

        let values: Vec<TestData> = collected
            .peek_value()
            .into_iter()
            .map(|item| item.value)
            .collect();
        assert!(!values.is_empty(), "expected at least one received sample");
    }
}
