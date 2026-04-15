//! Busy-spin Aeron subscriber node.
//!
//! Polls Aeron directly inside `cycle()` on the graph thread.  This is the
//! primary pattern for ultra-low latency: no thread boundary, no channel hop.
//!
//! Returns `Ok(true)` when at least one fragment arrived this cycle so that
//! the graph can propagate the tick reactively to downstream nodes.

use crate::adapters::aeron::transport::AeronSubscriberBackend;
use crate::{Burst, Element, GraphState, MutableNode, StreamPeekRef, UpStreams};

pub(crate) struct AeronSpinSubNode<T, F, B>
where
    T: Element,
    F: FnMut(&[u8]) -> Option<T>,
    B: AeronSubscriberBackend,
{
    backend: B,
    parser: F,
    value: Burst<T>,
}

impl<T, F, B> AeronSpinSubNode<T, F, B>
where
    T: Element,
    F: FnMut(&[u8]) -> Option<T>,
    B: AeronSubscriberBackend,
{
    pub(crate) fn new(backend: B, parser: F) -> Self {
        Self {
            backend,
            parser,
            value: Burst::new(),
        }
    }
}

impl<T, F, B> MutableNode for AeronSpinSubNode<T, F, B>
where
    T: Element,
    F: FnMut(&[u8]) -> Option<T> + 'static,
    B: AeronSubscriberBackend,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        let parser = &mut self.parser;
        let value = &mut self.value;
        self.backend.poll(&mut |fragment| {
            if let Some(v) = parser(fragment) {
                value.push(v);
            }
        })?;
        Ok(!self.value.is_empty())
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        // Always poll every graph cycle regardless of upstream activity.
        state.always_callback();
        Ok(())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }
}

impl<T, F, B> StreamPeekRef<Burst<T>> for AeronSpinSubNode<T, F, B>
where
    T: Element,
    F: FnMut(&[u8]) -> Option<T> + 'static,
    B: AeronSubscriberBackend,
{
    fn peek_ref(&self) -> &Burst<T> {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::aeron::transport::{MockSubscriber, i64_parser};
    use crate::{IntoStream, NanoTime, NodeOperators, RunFor, RunMode, StreamOperators};

    #[test]
    fn no_messages_returns_false_and_empty_burst() {
        let backend = MockSubscriber::new(vec![vec![]]);
        let node = AeronSpinSubNode::new(backend, i64_parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert!(stream.peek_value().is_empty());
    }

    #[test]
    fn single_message_triggers_downstream() {
        let msg = 42i64.to_le_bytes().to_vec();
        let backend = MockSubscriber::from_messages(vec![msg]);
        let node = AeronSpinSubNode::new(backend, i64_parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[42i64]);
    }

    #[test]
    fn multiple_messages_in_one_poll_collected_into_burst() {
        // One batch with three fragments — all delivered in the same cycle.
        let batch = vec![
            1i64.to_le_bytes().to_vec(),
            2i64.to_le_bytes().to_vec(),
            3i64.to_le_bytes().to_vec(),
        ];
        let backend = MockSubscriber::new(vec![batch]);
        let node = AeronSpinSubNode::new(backend, i64_parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[1i64, 2, 3]);
    }

    #[test]
    fn invalid_fragment_skipped_valid_fragment_kept() {
        let batch = vec![
            vec![0u8; 4],                 // too short → None
            42i64.to_le_bytes().to_vec(), // valid
        ];
        let backend = MockSubscriber::new(vec![batch]);
        let node = AeronSpinSubNode::new(backend, i64_parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[42i64]);
    }

    #[test]
    fn burst_clears_between_cycles() {
        // Cycle 1 has a message; cycle 2 has none.
        let backend = MockSubscriber::new(vec![vec![1i64.to_le_bytes().to_vec()], vec![]]);
        let node = AeronSpinSubNode::new(backend, i64_parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
            .unwrap();
        assert!(stream.peek_value().is_empty());
    }

    #[test]
    fn downstream_only_ticks_when_messages_arrive() {
        use crate::NanoTime;

        let backend = MockSubscriber::new(vec![
            vec![],                            // cycle 1: no data
            vec![7i64.to_le_bytes().to_vec()], // cycle 2: one message
            vec![],                            // cycle 3: no data
        ]);
        let node = AeronSpinSubNode::new(backend, i64_parser);
        let stream = node.into_stream();

        let tick_count = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let tc = tick_count.clone();

        let downstream = stream.inspect(move |_| tc.set(tc.get() + 1));
        downstream
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();

        // downstream should only tick once (cycle 2)
        assert_eq!(tick_count.get(), 1);
    }
}
