#![allow(dead_code)]
//! Common benchmark utilities.
//!
//! Provides shared helpers for Aeron transport benchmarks, following the project
//! convention to factor out common code into helper functions.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// Timeout for the media driver in milliseconds.
#[allow(dead_code)]
pub const DRIVER_TIMEOUT_MS: u64 = 30_000;

/// Sleep duration after starting the embedded media driver.
#[allow(dead_code)]
pub const DRIVER_INIT_SLEEP_MS: u64 = 200;

/// Sleep duration before stopping the media driver.
#[allow(dead_code)]
pub const DRIVER_STOP_SLEEP_MS: u64 = 100;

/// Timeout for blocking poll operations (publications/subscriptions).
#[allow(dead_code)]
pub const POLL_BLOCKING_TIMEOUT_SECS: u64 = 5;

/// Sleep duration after creating a publication or subscription.
#[allow(dead_code)]
pub const ENTITY_CREATION_SLEEP_MS: u64 = 100;

/// Term buffer length (16 MB).
#[allow(dead_code)]
pub const TERM_BUFFER_LENGTH: usize = 16 * 1024 * 1024;

/// RAII guard for managing Aeron media driver lifecycle in benchmarks.
///
/// The media driver is automatically started on creation and stopped on drop,
/// ensuring proper cleanup even if the benchmark panics.
pub struct MediaDriverGuard {
    stop_signal: Arc<AtomicBool>,
}

impl MediaDriverGuard {
    /// Starts an embedded Aeron media driver, or uses an external one.
    ///
    /// # Errors
    ///
    /// Returns an error if the media driver cannot be started.
    #[cfg(feature = "aeron-driver")]
    pub fn start() -> Result<Self, String> {
        // Check if user wants to use an external driver
        if std::env::var("AERON_EXTERNAL_DRIVER").is_ok() {
            eprintln!("Using external Aeron media driver (AERON_EXTERNAL_DRIVER is set)");
            return Ok(MediaDriverGuard {
                stop_signal: Arc::new(AtomicBool::new(false)),
            });
        }

        Self::start_embedded()
    }

    #[cfg(feature = "aeron-driver")]
    fn start_embedded() -> Result<Self, String> {
        use rusteron_media_driver::{AeronDriver, AeronDriverContext};

        // Clean up any stale Aeron state from previous runs
        if let Ok(aeron_dir) = std::env::var("AERON_DIR") {
            let _ = std::fs::remove_dir_all(&aeron_dir);
        } else {
            let _ = std::fs::remove_dir_all("/dev/shm/aeron");
            if let Ok(user) = std::env::var("USER") {
                let _ = std::fs::remove_dir_all(format!("/tmp/aeron-{}", user));
            }
        }

        let driver_context = AeronDriverContext::new().map_err(|e| {
            format!(
                "Failed to create media driver context: {:?}\n\
                 Ensure Aeron C libraries are installed, or set AERON_EXTERNAL_DRIVER=1 \
                 and run an external media driver.",
                e
            )
        })?;

        driver_context
            .set_driver_timeout_ms(DRIVER_TIMEOUT_MS)
            .map_err(|e| format!("Failed to set driver timeout: {:?}", e))?;

        driver_context
            .set_term_buffer_length(TERM_BUFFER_LENGTH)
            .map_err(|e| format!("Failed to set term buffer length: {:?}", e))?;
        driver_context
            .set_ipc_term_buffer_length(TERM_BUFFER_LENGTH)
            .map_err(|e| format!("Failed to set ipc term buffer length: {:?}", e))?;

        let (stop_signal, _driver_handle) = AeronDriver::launch_embedded(driver_context, false);
        thread::sleep(Duration::from_millis(DRIVER_INIT_SLEEP_MS));

        Ok(MediaDriverGuard { stop_signal })
    }

    #[cfg(not(feature = "aeron-driver"))]
    pub fn start() -> Result<Self, String> {
        Err("No driver feature enabled. Enable 'aeron-driver'.".to_string())
    }
}

impl Drop for MediaDriverGuard {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(DRIVER_STOP_SLEEP_MS));
    }
}

/// Default IPC channel for benchmarks.
pub const CHANNEL: &str = "aeron:ipc";

/// Message size variants for benchmarks.
#[derive(Debug, Clone, Copy)]
pub enum MessageSize {
    /// Small message (64 bytes) - typical for simple numeric data
    Small,
    /// Medium message (1024 bytes) - typical for structured messages
    Medium,
    /// Large message (8192 bytes) - typical for batch operations
    Large,
}

impl MessageSize {
    /// Returns the size in bytes.
    pub fn bytes(&self) -> usize {
        match self {
            MessageSize::Small => 64,
            MessageSize::Medium => 1024,
            MessageSize::Large => 8192,
        }
    }

    /// Creates a buffer of the specified size filled with test data.
    pub fn create_buffer(&self) -> Vec<u8> {
        vec![0xABu8; self.bytes()]
    }

    /// Returns a display name for the message size.
    pub fn name(&self) -> &'static str {
        match self {
            MessageSize::Small => "64B",
            MessageSize::Medium => "1KB",
            MessageSize::Large => "8KB",
        }
    }
}

// ============================================================================
// Rusteron benchmark helpers
// ============================================================================

#[cfg(feature = "aeron-driver")]
pub mod rusteron_support {
    use super::*;
    use rusteron_client::IntoCString;

    /// Benchmark context holding the media driver and Aeron client.
    ///
    /// Use this to set up benchmarks with a single shared driver and client.
    #[allow(dead_code)]
    pub struct BenchContext {
        pub aeron: rusteron_client::Aeron,
        pub driver: MediaDriverGuard,
    }

    #[allow(dead_code)]
    impl BenchContext {
        /// Creates a new benchmark context with an embedded media driver and Aeron client.
        pub fn new() -> Option<Self> {
            let driver = match MediaDriverGuard::start() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Skipping benchmark: {}", e);
                    return None;
                }
            };

            let context =
                rusteron_client::AeronContext::new().expect("Failed to create Aeron context");
            let aeron = rusteron_client::Aeron::new(&context).expect("Failed to create Aeron");
            aeron.start().expect("Failed to start Aeron");

            Some(BenchContext { aeron, driver })
        }

        /// Adds a publication and waits for it to be ready.
        pub fn add_publication(&self, stream_id: i32) -> rusteron_client::AeronPublication {
            let async_pub = self
                .aeron
                .async_add_publication(&CHANNEL.into_c_string(), stream_id)
                .expect("Failed to start publication");

            let publication = async_pub
                .poll_blocking(Duration::from_secs(POLL_BLOCKING_TIMEOUT_SECS))
                .expect("Failed to complete publication");

            thread::sleep(Duration::from_millis(ENTITY_CREATION_SLEEP_MS));
            publication
        }

        /// Adds a subscription and waits for it to be ready.
        pub fn add_subscription(&self, stream_id: i32) -> rusteron_client::AeronSubscription {
            let async_sub = self
                .aeron
                .async_add_subscription(
                    &CHANNEL.into_c_string(),
                    stream_id,
                    rusteron_client::Handlers::no_available_image_handler(),
                    rusteron_client::Handlers::no_unavailable_image_handler(),
                )
                .expect("Failed to start subscription");

            let subscription = async_sub
                .poll_blocking(Duration::from_secs(POLL_BLOCKING_TIMEOUT_SECS))
                .expect("Failed to complete subscription");

            thread::sleep(Duration::from_millis(ENTITY_CREATION_SLEEP_MS));
            subscription
        }

        /// Adds both a publication and subscription on the same stream.
        pub fn add_pub_sub(
            &self,
            stream_id: i32,
        ) -> (
            rusteron_client::AeronPublication,
            rusteron_client::AeronSubscription,
        ) {
            let publication = self.add_publication(stream_id);
            let subscription = self.add_subscription(stream_id);
            (publication, subscription)
        }
    }
}
