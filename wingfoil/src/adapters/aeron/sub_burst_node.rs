//! Typed-parser Aeron subscriber nodes — parallel-additive surface.
//!
//! Backs [`aeron_sub_burst`](super::aeron_sub_burst) — the typed-parser
//! evolution of the bytes-only [`aeron_sub`](super::aeron_sub) factory.
//! The new surface exposes Aeron's per-fragment header (`position`,
//! `session_id`, `stream_id`) and lets parsers signal recoverable errors via
//! [`TransportError`] instead of collapsing "valid but skip" and "malformed"
//! into a single `None`.
//!
//! # Two structs, not one enum
//!
//! Spin and threaded modes live as two distinct types
//! ([`AeronSpinSubBurstNode`] and the `build_threaded` factory) mirroring
//! [`super::sub_spin`] / [`super::sub_threaded`]. The split keeps `cycle()`
//! bodies linear (the spin variant polls inline; the threaded variant
//! delegates to [`ReceiverStream`](crate::nodes::receiver::ReceiverStream))
//! and matches the existing module layout one-for-one. Story 12.5 will add
//! status-stream wiring to the threaded variant, at which point the two
//! shapes diverge further — abstraction over them is an explicit non-goal.
//!
//! # Existing surface is untouched
//!
//! `aeron_sub` / `aeron_sub_with_options` and their bytes-parser `Fn(&[u8])
//! -> Option<T>` shape remain bit-identical. The new surface is parallel and
//! additive: backends pick up the typed surface via a defaulted
//! [`AeronSubscriberBackend::poll_fragments`] method.

use crate::adapters::aeron::buffer::FragmentBuffer;
use crate::adapters::aeron::error::TransportError;
use crate::adapters::aeron::transport::AeronSubscriberBackend;
use crate::channel::{ChannelSender, Message};
use crate::nodes::receiver::ReceiverStream;
use crate::{
    Burst, Element, GraphState, IntoStream, MutableNode, Stream, StreamPeekRef, UpStreams,
};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tinyvec::TinyVec;

// ---------------------------------------------------------------------------
// AeronSpinSubBurstNode<T, F, B>
// ---------------------------------------------------------------------------

/// Busy-spin typed-parser Aeron subscriber node.
///
/// Polls Aeron via [`AeronSubscriberBackend::poll_fragments`] inside
/// `cycle()` on the graph thread. Parser errors are logged and dropped — a
/// malformed fragment never aborts the cycle (NFR5 zero-stopping rule).
pub(crate) struct AeronSpinSubBurstNode<T, F, B>
where
    T: Element,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError>,
    B: AeronSubscriberBackend,
{
    backend: B,
    parser: F,
    value: Burst<T>,
}

impl<T, F, B> AeronSpinSubBurstNode<T, F, B>
where
    T: Element,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError>,
    B: AeronSubscriberBackend,
{
    #[must_use]
    pub(crate) fn new(backend: B, parser: F) -> Self {
        Self {
            backend,
            parser,
            value: Burst::new(),
        }
    }
}

impl<T, F, B> MutableNode for AeronSpinSubBurstNode<T, F, B>
where
    T: Element,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError> + 'static,
    B: AeronSubscriberBackend,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        let parser = &mut self.parser;
        let value = &mut self.value;
        self.backend.poll_fragments(&mut |frag| match parser(frag) {
            Ok(Some(v)) => value.push(v),
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "[wingfoil::adapters::aeron] WARN parser dropped fragment at position {}: {e}",
                    frag.position()
                );
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

impl<T, F, B> StreamPeekRef<Burst<T>> for AeronSpinSubBurstNode<T, F, B>
where
    T: Element,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError> + 'static,
    B: AeronSubscriberBackend,
{
    fn peek_ref(&self) -> &Burst<T> {
        &self.value
    }
}

// ---------------------------------------------------------------------------
// Threaded burst node
// ---------------------------------------------------------------------------

/// Wraps [`ReceiverStream`] to add a cooperative stop signal for the
/// background polling thread (parallel to `sub_threaded::ThreadedAeronNode`).
struct ThreadedAeronBurstNode<T: Element + Send> {
    inner: ReceiverStream<T>,
    stop_flag: Arc<AtomicBool>,
}

impl<T: Element + Send> MutableNode for ThreadedAeronBurstNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.inner.cycle(state)
    }

    fn upstreams(&self) -> UpStreams {
        self.inner.upstreams()
    }

    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.inner.setup(state)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.inner.start(state)
    }

    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.inner.stop(state).ok();
        Ok(())
    }

    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.inner.teardown(state)
    }
}

impl<T: Element + Send> StreamPeekRef<TinyVec<[T; 1]>> for ThreadedAeronBurstNode<T> {
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        self.inner.peek_ref()
    }
}

/// Build a threaded typed-parser Aeron subscriber stream.
///
/// Mirrors [`super::sub_threaded::build`] but uses the typed-parser /
/// typed-header [`AeronSubscriberBackend::poll_fragments`] surface and the
/// `Result<Option<T>, TransportError>` parser signature.
pub(crate) fn build_threaded<T, F, B>(backend: B, parser: F) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError> + Send + 'static,
    B: AeronSubscriberBackend,
{
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_thread = stop_flag.clone();

    let state = Mutex::new(Some((backend, parser)));
    let inner = ReceiverStream::new(
        move |sender: ChannelSender<T>, _stop: Arc<AtomicBool>| {
            let mut state_guard = state.lock().expect("state lock poisoned");
            let (mut backend, mut parser) = state_guard
                .take()
                .expect("threaded aeron burst closure called more than once");
            drop(state_guard);

            let mut idle_count = 0u32;
            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let count = backend.poll_fragments(&mut |frag| match parser(frag) {
                    Ok(Some(v)) => {
                        let _ = sender.send_message(Message::RealtimeValue(v));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!(
                            "[wingfoil::adapters::aeron] WARN parser dropped fragment at position {}: {e}",
                            frag.position()
                        );
                    }
                })?;

                if count == 0 {
                    idle_count = (idle_count + 1).min(20);
                    let micros = 1u64 << idle_count.min(10);
                    std::thread::sleep(Duration::from_micros(micros));
                } else {
                    idle_count = 0;
                }
            }
        },
        true,
    );

    ThreadedAeronBurstNode { inner, stop_flag }.into_stream()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::aeron::transport::MockSubscriber;
    use crate::{IntoStream, NanoTime, NodeOperators, RunFor, RunMode};

    /// Typed-parser sibling of `transport::i64_parser` for the burst surface.
    fn i64_parser_typed(f: &FragmentBuffer<'_>) -> Result<Option<i64>, TransportError> {
        Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
    }

    #[test]
    fn given_burst_spin_node_when_no_fragments_then_empty_burst_and_returns_false_active() {
        let backend = MockSubscriber::new(vec![vec![]]);
        let node = AeronSpinSubBurstNode::new(backend, i64_parser_typed);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert!(stream.peek_value().is_empty());
    }

    #[test]
    fn given_burst_spin_node_when_single_fragment_then_one_element_burst() {
        let msg = 42i64.to_le_bytes().to_vec();
        let backend = MockSubscriber::from_messages(vec![msg]);
        let node = AeronSpinSubBurstNode::new(backend, i64_parser_typed);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[42i64]);
    }

    #[test]
    fn given_burst_spin_node_when_three_fragments_one_poll_then_three_element_burst() {
        let batch = vec![
            1i64.to_le_bytes().to_vec(),
            2i64.to_le_bytes().to_vec(),
            3i64.to_le_bytes().to_vec(),
        ];
        let backend = MockSubscriber::new(vec![batch]);
        let node = AeronSpinSubBurstNode::new(backend, i64_parser_typed);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[1i64, 2, 3]);
    }

    #[test]
    fn given_burst_spin_node_when_parser_returns_none_then_fragment_dropped() {
        // Wrong-length first fragment → `try_into` fails → `Ok(None)`.
        let batch = vec![
            vec![0u8; 4],                 // 4 bytes — i64 try_into is None
            42i64.to_le_bytes().to_vec(), // valid
        ];
        let backend = MockSubscriber::new(vec![batch]);
        let node = AeronSpinSubBurstNode::new(backend, i64_parser_typed);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[42i64]);
    }

    #[test]
    fn given_burst_spin_node_when_parser_returns_err_then_fragment_dropped_and_cycle_continues() {
        // Middle fragment yields TransportError::Invalid; valid fragments
        // either side MUST still be collected.  Parser detects the "poison"
        // marker by length rather than content.
        let batch = vec![
            1i64.to_le_bytes().to_vec(),
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD], // 6 bytes — neither 8 nor 4
            3i64.to_le_bytes().to_vec(),
        ];
        let backend = MockSubscriber::new(vec![batch]);
        let parser = |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            match f.as_ref().len() {
                8 => Ok(Some(i64::from_le_bytes(f.as_ref().try_into().unwrap()))),
                6 => Err(TransportError::Invalid("bad".into())),
                _ => Ok(None),
            }
        };
        let node = AeronSpinSubBurstNode::new(backend, parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[1i64, 3]);
    }

    #[test]
    fn given_burst_spin_node_when_burst_completes_then_clears_before_next_cycle() {
        let backend = MockSubscriber::new(vec![vec![1i64.to_le_bytes().to_vec()], vec![]]);
        let node = AeronSpinSubBurstNode::new(backend, i64_parser_typed);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
            .unwrap();
        assert!(stream.peek_value().is_empty());
    }

    #[test]
    fn given_burst_spin_node_when_parser_inspects_fragment_header_then_default_header_is_zero() {
        let backend = MockSubscriber::from_messages(vec![7i64.to_le_bytes().to_vec()]);
        // Parser asserts the synthesised-default-header values inline so a
        // failure surfaces as a panic during the graph run.
        let parser = |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            assert_eq!(f.position(), 0);
            assert_eq!(f.header().session_id, 0);
            assert_eq!(f.header().stream_id, 0);
            Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
        };
        let node = AeronSpinSubBurstNode::new(backend, parser);
        let stream = node.into_stream();
        stream
            .clone()
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(stream.peek_value().as_slice(), &[7i64]);
    }
}
