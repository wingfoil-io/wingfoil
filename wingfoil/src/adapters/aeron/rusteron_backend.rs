//! Rusteron (C++ FFI) backend for the Aeron adapter.
//!
//! Requires the `aeron-rusteron` feature flag and a C++ toolchain with
//! the Aeron C++ library headers available on the build machine.
//!
//! # Connection lifecycle
//!
//! An [`AeronHandle`] owns the `AeronContext` + `Aeron` objects and must
//! remain alive for the lifetime of all subscriptions and publications
//! derived from it.  Build one with [`AeronHandle::connect`] before
//! constructing subscribers or publishers.

use crate::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};
use rusteron_client::{
    Aeron, AeronContext, AeronPublication, AeronSubscription, Handlers, IntoCString,
};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Connection handle
// ---------------------------------------------------------------------------

/// Owns the `AeronContext` and `Aeron` client instance.
///
/// Keep this value alive for as long as any subscriber or publisher derived
/// from it is in use.
pub struct AeronHandle {
    // `_context` must be held alive; `aeron` borrows from it.
    _context: AeronContext,
    pub(crate) aeron: Aeron,
}

impl AeronHandle {
    /// Connect to the Aeron media driver using default context settings.
    pub fn connect() -> anyhow::Result<Self> {
        let context = AeronContext::new()?;
        let aeron = Aeron::new(&context)?;
        aeron.start()?;
        Ok(Self {
            _context: context,
            aeron,
        })
    }

    /// Add a subscription on the given channel + stream.
    ///
    /// Blocks until the subscription is established or `timeout` elapses.
    pub fn subscription(
        &self,
        channel: &str,
        stream_id: i32,
        timeout: Duration,
    ) -> anyhow::Result<RusteronSubscriber> {
        let sub = self
            .aeron
            .async_add_subscription(
                &channel.into_c_string(),
                stream_id,
                Handlers::no_available_image_handler(),
                Handlers::no_unavailable_image_handler(),
            )?
            .poll_blocking(timeout)?;
        Ok(RusteronSubscriber { sub })
    }

    /// Add a publication on the given channel + stream.
    ///
    /// Blocks until the publication is established or `timeout` elapses.
    pub fn publication(
        &self,
        channel: &str,
        stream_id: i32,
        timeout: Duration,
    ) -> anyhow::Result<RusteronPublisher> {
        let pub_ = self
            .aeron
            .async_add_publication(&channel.into_c_string(), stream_id)?
            .poll_blocking(timeout)?;
        Ok(RusteronPublisher { pub_ })
    }
}

// ---------------------------------------------------------------------------
// Subscriber
// ---------------------------------------------------------------------------

pub struct RusteronSubscriber {
    sub: AeronSubscription,
}

impl AeronSubscriberBackend for RusteronSubscriber {
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
        let mut count = 0usize;
        // rusteron fragment handler closure: receives (buffer, header)
        self.sub.poll(
            |buffer, _header| {
                handler(buffer);
                count += 1;
            },
            256, // fragment limit per poll
        )?;
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Publisher
// ---------------------------------------------------------------------------

pub struct RusteronPublisher {
    pub_: AeronPublication,
}

impl AeronPublisherBackend for RusteronPublisher {
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()> {
        let position = self.pub_.offer(buffer)?;
        if position < 0 {
            anyhow::bail!("Aeron offer back-pressure: position={position}");
        }
        Ok(())
    }
}
