//! Backend-agnostic transport traits for Aeron, plus a mock for unit tests.

use crate::adapters::aeron::buffer::ClaimBuffer;
use crate::adapters::aeron::error::TransportError;

/// Subscribes to an Aeron channel, polling fragments non-blocking.
pub trait AeronSubscriberBackend: Send + 'static {
    /// Poll for available fragments, calling `handler` for each one.
    /// Non-blocking.  Returns the number of fragments processed.
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize>;

    /// Returns whether this subscription is currently connected to at least
    /// one publication.
    ///
    /// Default returns `false`; backends with subscriber-side state should
    /// override.
    fn is_connected(&self) -> bool {
        false
    }

    /// Returns whether this subscription has been closed.
    ///
    /// A closed subscription has had its lifecycle ended (gracefully or via
    /// shutdown). This is distinct from being temporarily disconnected —
    /// closed is terminal.
    fn is_closed(&self) -> bool {
        false
    }
}

/// Publishes bytes to an Aeron channel.
pub trait AeronPublisherBackend: 'static {
    /// Offer a buffer to the publication.
    /// Non-blocking; returns back-pressure errors via `Err` rather than blocking.
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()>;

    /// Returns whether this publication is currently connected to at least
    /// one subscriber.
    fn is_connected(&self) -> bool {
        false
    }

    /// Returns whether this publication has been closed.
    fn is_closed(&self) -> bool {
        false
    }

    /// Claims a buffer for direct-write zero-copy message publication.
    ///
    /// Default impl returns [`TransportError::Invalid`] — backends that
    /// support zero-copy publication (e.g. rusteron) override this method.
    fn try_claim<'a>(&'a mut self, _length: usize) -> Result<ClaimBuffer<'a>, TransportError> {
        Err(TransportError::Invalid(
            "try_claim not supported on this backend".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Mock backends — only compiled in test builds
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) struct MockSubscriber {
    batches: std::collections::VecDeque<Vec<Vec<u8>>>,
}

#[cfg(test)]
impl MockSubscriber {
    /// Each inner `Vec<Vec<u8>>` is one poll-batch: all fragments are delivered
    /// in a single `poll()` call.
    pub(crate) fn new(batches: Vec<Vec<Vec<u8>>>) -> Self {
        Self {
            batches: batches.into(),
        }
    }

    /// Convenience: wrap every message as its own single-fragment batch.
    pub(crate) fn from_messages(messages: Vec<Vec<u8>>) -> Self {
        Self::new(messages.into_iter().map(|m| vec![m]).collect())
    }
}

#[cfg(test)]
impl AeronSubscriberBackend for MockSubscriber {
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
        let batch = self.batches.pop_front().unwrap_or_default();
        let count = batch.len();
        for msg in &batch {
            handler(msg);
        }
        Ok(count)
    }
}

#[cfg(test)]
pub(crate) struct MockPublisher {
    pub(crate) published: Vec<Vec<u8>>,
}

#[cfg(test)]
impl MockPublisher {
    pub(crate) fn new() -> Self {
        Self {
            published: Vec::new(),
        }
    }
}

#[cfg(test)]
impl AeronPublisherBackend for MockPublisher {
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()> {
        self.published.push(buffer.to_vec());
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn i64_parser(bytes: &[u8]) -> Option<i64> {
    bytes.try_into().ok().map(i64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_default_subscriber_backend_when_is_connected_called_then_returns_false() {
        let sub = MockSubscriber::from_messages(vec![]);
        assert!(!sub.is_connected());
    }

    #[test]
    fn given_default_subscriber_backend_when_is_closed_called_then_returns_false() {
        let sub = MockSubscriber::from_messages(vec![]);
        assert!(!sub.is_closed());
    }

    #[test]
    fn given_default_publisher_backend_when_is_connected_called_then_returns_false() {
        let pub_ = MockPublisher::new();
        assert!(!pub_.is_connected());
    }

    #[test]
    fn given_default_publisher_backend_when_is_closed_called_then_returns_false() {
        let pub_ = MockPublisher::new();
        assert!(!pub_.is_closed());
    }

    #[test]
    fn given_default_publisher_backend_when_try_claim_called_then_returns_invalid_error() {
        let mut pub_ = MockPublisher::new();
        let err = pub_
            .try_claim(64)
            .expect_err("default try_claim returns Err");
        match err {
            TransportError::Invalid(msg) => {
                assert!(
                    msg.contains("try_claim"),
                    "default Invalid message references try_claim: {msg}"
                );
            }
            other => panic!("expected TransportError::Invalid, got {other:?}"),
        }
    }
}
