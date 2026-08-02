//! Rusteron (C++ FFI) backend for the Aeron adapter.
//!
//! Requires the `aeron` feature flag and a C++ toolchain with
//! the Aeron C++ library headers available on the build machine.
//!
//! # Connection lifecycle
//!
//! An [`AeronHandle`] owns the `AeronContext` + `Aeron` objects and must
//! remain alive for the lifetime of all subscriptions and publications
//! derived from it.  Build one with [`AeronHandle::connect`] before
//! constructing subscribers or publishers.

use crate::adapters::aeron::DEFAULT_FRAGMENT_LIMIT;
use crate::adapters::aeron::buffer::{ClaimBuffer, FragmentBuffer, FragmentHeader};
use crate::adapters::aeron::error::TransportError;
use crate::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};
use rusteron_client::{
    Aeron, AeronBufferClaim, AeronContext, AeronPublication, AeronSubscription, Handlers,
    IntoCString,
};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Rusteron i64 → TransportError mapping
// ---------------------------------------------------------------------------

// Negative-code mapping table verbatim from
// aerofoil/src/transport/rusteron/error.rs.
//
// Rusteron's `AeronPublication::try_claim` / `offer` return an `i64`:
//   - Positive values are a stream position (success).
//   - Negative values are wire-level Aeron error codes:
//       -1: Not connected
//       -2: Back pressure
//       -4: Publication closed
//   - Any other negative code is surfaced as a `Backend` error with the code
//     embedded for diagnostics.
fn result_to_transport_error(result: i64) -> Result<i64, TransportError> {
    match result {
        pos if pos >= 0 => Ok(pos),
        -2 => Err(TransportError::BackPressure),
        -1 => Err(TransportError::Connection("Not connected".to_string())),
        -4 => Err(TransportError::Connection("Publication closed".to_string())),
        code => Err(TransportError::Backend(format!(
            "Rusteron error code: {}",
            code
        ))),
    }
}

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
    aeron: Aeron,
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
        Ok(RusteronSubscriber {
            sub,
            fragment_limit: DEFAULT_FRAGMENT_LIMIT,
        })
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
        let publication = self
            .aeron
            .async_add_publication(&channel.into_c_string(), stream_id)?
            .poll_blocking(timeout)?;
        Ok(RusteronPublisher { publication })
    }
}

// ---------------------------------------------------------------------------
// Subscriber
// ---------------------------------------------------------------------------

pub struct RusteronSubscriber {
    sub: AeronSubscription,
    /// Cap on fragments delivered per [`AeronSubscriberBackend::poll`] call.
    ///
    /// Aeron's `poll`/`poll_once` treats this as a **cap, not a target**: the
    /// call returns control after at most `fragment_limit` fragments OR when
    /// no more are immediately available. Defaults to `256` (Aeron sample
    /// harness convention). Lower values reduce per-cycle latency tail but
    /// add per-fragment loop overhead; higher values amortise loop overhead
    /// but lengthen worst-case `poll()` cycles.
    fragment_limit: usize,
}

impl RusteronSubscriber {
    /// Wrap an existing rusteron [`AeronSubscription`] in a wingfoil-typed
    /// subscriber. Useful for advanced setups (custom driver lifecycle,
    /// benchmarks) where the subscription is acquired outside
    /// [`AeronHandle::subscription`].
    #[must_use]
    pub fn new(sub: AeronSubscription) -> Self {
        Self {
            sub,
            fragment_limit: DEFAULT_FRAGMENT_LIMIT,
        }
    }

    /// Override the per-`poll()` fragment cap.
    ///
    /// Chain after [`AeronHandle::subscription`]:
    /// `handle.subscription(...).?.with_fragment_limit(32)`.
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

/// `RusteronSubscriber` mirrors `AeronSubscription`'s connection state to the
/// trait surface — rusteron exposes both `is_connected()` and `is_closed()`
/// on the subscription handle (per aerofoil 11.4's adoption pattern).
impl AeronSubscriberBackend for RusteronSubscriber {
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
        let mut count = 0usize;
        // rusteron fragment handler closure: receives (buffer, header)
        self.sub.poll_once(
            |buffer, _header| {
                handler(buffer);
                count += 1;
            },
            self.fragment_limit,
        )?;
        Ok(count)
    }

    fn poll_fragments(
        &mut self,
        handler: &mut dyn FnMut(&FragmentBuffer<'_>),
    ) -> Result<usize, TransportError> {
        let mut count = 0usize;
        self.sub
            .poll_once(
                |buffer, header| {
                    // `position()` is a direct method; session_id / stream_id
                    // live on the values-frame.  An FFI failure on values
                    // lookup degrades silently to zeros — preserves the
                    // "never panic in the fast path" rule (NFR5).
                    let (session_id, stream_id) = header
                        .get_values()
                        .ok()
                        .map(|v| {
                            let frame = v.frame();
                            (frame.session_id(), frame.stream_id())
                        })
                        .unwrap_or((0, 0));
                    let frag_header = FragmentHeader {
                        position: header.position(),
                        session_id,
                        stream_id,
                    };
                    let frag = FragmentBuffer::new(buffer, frag_header);
                    handler(&frag);
                    count += 1;
                },
                self.fragment_limit,
            )
            .map_err(|e| TransportError::Backend(format!("{e:#}")))?;
        Ok(count)
    }

    fn is_connected(&self) -> bool {
        self.sub.is_connected()
    }

    fn is_closed(&self) -> bool {
        self.sub.is_closed()
    }

    fn with_fragment_limit(self, fragment_limit: usize) -> Self {
        RusteronSubscriber::with_fragment_limit(self, fragment_limit)
    }
}

// ---------------------------------------------------------------------------
// Publisher
// ---------------------------------------------------------------------------

pub struct RusteronPublisher {
    publication: AeronPublication,
}

impl RusteronPublisher {
    /// Wrap an existing rusteron [`AeronPublication`] in a wingfoil-typed
    /// publisher. Useful for advanced setups (custom driver lifecycle,
    /// benchmarks) where the publication is acquired outside
    /// [`AeronHandle::publication`].
    #[must_use]
    pub fn new(publication: AeronPublication) -> Self {
        Self { publication }
    }
}

impl AeronPublisherBackend for RusteronPublisher {
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()> {
        let position = self
            .publication
            .offer(buffer, Handlers::no_reserved_value_supplier_handler());
        // Route through the shared code→error mapping so back-pressure surfaces
        // as a typed `TransportError::BackPressure` (which the publisher node
        // downcasts to record `AeronStatus::BackPressured`), rather than an
        // opaque string error that the node can't distinguish from a fatal one.
        result_to_transport_error(position)?;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.publication.is_connected()
    }

    fn is_closed(&self) -> bool {
        self.publication.is_closed()
    }

    fn try_claim<'a>(&'a mut self, length: usize) -> Result<ClaimBuffer<'a>, TransportError> {
        let claim = AeronBufferClaim::default();
        let position = self.publication.try_claim(length, &claim);
        let position = result_to_transport_error(position)?;
        Ok(ClaimBuffer::from_aeron(claim, position))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_positive_result_when_mapped_then_returns_ok_position() {
        assert!(matches!(result_to_transport_error(12345), Ok(12345)));
        assert!(matches!(result_to_transport_error(0), Ok(0)));
    }

    #[test]
    fn given_minus_two_when_mapped_then_returns_back_pressure() {
        let err = result_to_transport_error(-2).expect_err("-2 maps to Err");
        assert!(matches!(err, TransportError::BackPressure));
    }

    #[test]
    fn given_minus_one_when_mapped_then_returns_connection_not_connected() {
        let err = result_to_transport_error(-1).expect_err("-1 maps to Err");
        match err {
            TransportError::Connection(msg) => assert!(
                msg.contains("Not connected"),
                "expected 'Not connected', got: {msg}"
            ),
            other => panic!("expected Connection, got {other:?}"),
        }
    }

    #[test]
    fn given_minus_four_when_mapped_then_returns_connection_publication_closed() {
        let err = result_to_transport_error(-4).expect_err("-4 maps to Err");
        match err {
            TransportError::Connection(msg) => assert!(
                msg.contains("Publication closed"),
                "expected 'Publication closed', got: {msg}"
            ),
            other => panic!("expected Connection, got {other:?}"),
        }
    }

    #[test]
    fn given_unknown_negative_code_when_mapped_then_returns_backend_with_code() {
        let err = result_to_transport_error(-99).expect_err("-99 maps to Err");
        match err {
            TransportError::Backend(msg) => {
                assert!(msg.contains("-99"), "expected code in message, got: {msg}")
            }
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    // Field-round-trip tests for `fragment_limit` require a live
    // `AeronSubscription` (an FFI handle that cannot be zero-initialised),
    // so they are gated behind `aeron-integration-test` and rely on the
    // shared `start_media_driver()` helper.
    #[cfg(feature = "aeron-integration-test")]
    mod field_tests {
        use super::*;
        use crate::adapters::aeron::integration_tests::{
            AERON_CHANNEL, CONNECT_TIMEOUT, start_media_driver,
        };

        #[test]
        fn given_rusteron_subscriber_when_built_default_then_fragment_limit_is_256()
        -> anyhow::Result<()> {
            let _container = start_media_driver()?;
            let handle = AeronHandle::connect()?;
            let sub = handle.subscription(AERON_CHANNEL, 2101, CONNECT_TIMEOUT)?;
            assert_eq!(sub.fragment_limit(), 256);
            Ok(())
        }

        #[test]
        fn given_rusteron_subscriber_when_with_fragment_limit_then_field_matches()
        -> anyhow::Result<()> {
            let _container = start_media_driver()?;
            let handle = AeronHandle::connect()?;
            let sub = handle
                .subscription(AERON_CHANNEL, 2102, CONNECT_TIMEOUT)?
                .with_fragment_limit(32);
            assert_eq!(sub.fragment_limit(), 32);
            Ok(())
        }
    }
}
