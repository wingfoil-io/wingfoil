//! Backend-agnostic transport traits for Aeron, plus a mock for unit tests.

/// Subscribes to an Aeron channel, polling fragments non-blocking.
pub trait AeronSubscriberBackend: Send + 'static {
    /// Poll for available fragments, calling `handler` for each one.
    /// Non-blocking.  Returns the number of fragments processed.
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize>;
}

/// Publishes bytes to an Aeron channel.
pub trait AeronPublisherBackend: 'static {
    /// Offer a buffer to the publication.
    /// Non-blocking; returns back-pressure errors via `Err` rather than blocking.
    fn offer(&mut self, buffer: &[u8]) -> anyhow::Result<()>;
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
