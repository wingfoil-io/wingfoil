//! Pure-Rust `aeron-rs` backend for the Aeron adapter.
//!
//! Requires the `aeron-rs` feature flag.  No C++ toolchain needed.
//!
//! `aeron-rs` is less mature than the rusteron C++ FFI backend.  Prefer
//! `aeron-rusteron` for production use.
//!
//! # Connection lifecycle
//!
//! Build an [`AeronRsHandle`] with [`AeronRsHandle::connect`], then derive
//! subscribers and publishers from it.

use crate::adapters::aeron::DEFAULT_FRAGMENT_LIMIT;
use crate::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};
use aeron_rs::aeron::Aeron;
use aeron_rs::concurrent::atomic_buffer::{AlignedBuffer, AtomicBuffer};
use aeron_rs::concurrent::logbuffer::header::Header;
use aeron_rs::context::Context;
use aeron_rs::publication::Publication;
use aeron_rs::subscription::Subscription;
use aeron_rs::utils::types::Index;
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn poll_until_found<T, F>(mut check: F, timeout: Duration, what: &str) -> anyhow::Result<T>
where
    F: FnMut() -> Option<T>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(item) = check() {
            return Ok(item);
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// Connection handle
// ---------------------------------------------------------------------------

pub struct AeronRsHandle {
    aeron: Arc<Mutex<Aeron>>,
}

impl AeronRsHandle {
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn connect() -> anyhow::Result<Self> {
        let mut ctx = Context::new();
        // Honour the same AERON_DIR env var that the C driver uses.
        if let Ok(dir) = std::env::var("AERON_DIR") {
            ctx.set_aeron_dir(dir);
        }
        let aeron = Aeron::new(ctx)?;
        Ok(Self {
            aeron: Arc::new(Mutex::new(aeron)),
        })
    }

    pub fn subscription(
        &self,
        channel: &str,
        stream_id: i32,
        timeout: Duration,
    ) -> anyhow::Result<AeronRsSubscriber> {
        let mut aeron = self.aeron.lock().unwrap();
        let id = aeron.add_subscription(CString::new(channel)?, stream_id)?;
        let sub = poll_until_found(
            || aeron.find_subscription(id).ok(),
            timeout,
            &format!("subscription on {channel}:{stream_id}"),
        )?;
        Ok(AeronRsSubscriber {
            sub,
            fragment_limit: DEFAULT_FRAGMENT_LIMIT,
        })
    }

    pub fn publication(
        &self,
        channel: &str,
        stream_id: i32,
        timeout: Duration,
    ) -> anyhow::Result<AeronRsPublisher> {
        let mut aeron = self.aeron.lock().unwrap();
        let id = aeron.add_publication(CString::new(channel)?, stream_id)?;
        let publication = poll_until_found(
            || aeron.find_publication(id).ok(),
            timeout,
            &format!("publication on {channel}:{stream_id}"),
        )?;
        Ok(AeronRsPublisher { publication })
    }
}

// ---------------------------------------------------------------------------
// Subscriber
// ---------------------------------------------------------------------------

pub struct AeronRsSubscriber {
    sub: Arc<Mutex<Subscription>>,
    /// Cap on fragments delivered per [`AeronSubscriberBackend::poll`] call.
    ///
    /// Aeron's `poll` treats this as a **cap, not a target**: the call returns
    /// control after at most `fragment_limit` fragments OR when no more are
    /// immediately available. Defaults to `256` (Aeron sample harness
    /// convention). Lower values reduce per-cycle latency tail but add
    /// per-fragment loop overhead; higher values amortise loop overhead but
    /// lengthen worst-case `poll()` cycles. Stored as `usize` for API parity
    /// with rusteron; cast to `Index`/`i32` at the FFI call site.
    fragment_limit: usize,
}

impl AeronRsSubscriber {
    /// Override the per-`poll()` fragment cap.
    #[must_use]
    pub fn with_fragment_limit(mut self, fragment_limit: usize) -> Self {
        self.fragment_limit = fragment_limit;
        self
    }

    /// Returns the current per-`poll()` fragment cap.
    #[must_use]
    pub fn fragment_limit(&self) -> usize {
        self.fragment_limit
    }
}

impl AeronSubscriberBackend for AeronRsSubscriber {
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
        let mut count = 0usize;
        let mut sub = self.sub.lock().unwrap();
        sub.poll(
            &mut |buffer: &AtomicBuffer, offset: Index, length: Index, _header: &Header| {
                handler(buffer.as_sub_slice(offset, length));
                count += 1;
            },
            self.fragment_limit as Index,
        );
        Ok(count)
    }

    fn with_fragment_limit(self, fragment_limit: usize) -> Self {
        AeronRsSubscriber::with_fragment_limit(self, fragment_limit)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Field-round-trip tests for `fragment_limit` require a live aeron-rs
// `Subscription` (an FFI handle that cannot be zero-initialised), so they
// are gated behind `aeron-integration-test` and rely on the shared
// `start_media_driver()` helper.
#[cfg(all(test, feature = "aeron-integration-test"))]
mod tests {
    use super::*;
    use crate::adapters::aeron::integration_tests::{
        AERON_CHANNEL, CONNECT_TIMEOUT, start_media_driver,
    };

    #[test]
    fn given_aeron_rs_subscriber_when_built_default_then_fragment_limit_is_256()
    -> anyhow::Result<()> {
        let _container = start_media_driver()?;
        let handle = AeronRsHandle::connect()?;
        let sub = handle.subscription(AERON_CHANNEL, 3101, CONNECT_TIMEOUT)?;
        assert_eq!(sub.fragment_limit(), 256);
        Ok(())
    }

    #[test]
    fn given_aeron_rs_subscriber_when_with_fragment_limit_then_field_matches() -> anyhow::Result<()>
    {
        let _container = start_media_driver()?;
        let handle = AeronRsHandle::connect()?;
        let sub = handle
            .subscription(AERON_CHANNEL, 3102, CONNECT_TIMEOUT)?
            .with_fragment_limit(64);
        assert_eq!(sub.fragment_limit(), 64);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Publisher
// ---------------------------------------------------------------------------

pub struct AeronRsPublisher {
    publication: Arc<Mutex<Publication>>,
}

impl AeronPublisherBackend for AeronRsPublisher {
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()> {
        let publication = self.publication.lock().unwrap();
        let len = buffer.len() as Index;
        let aligned = AlignedBuffer::with_capacity(len);
        let ab = AtomicBuffer::from_aligned(&aligned);
        ab.put_bytes(0, buffer);
        publication.offer_part(ab, 0, len)?;
        Ok(())
    }
}
