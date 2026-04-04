//! Threaded Aeron subscriber (secondary pattern).
//!
//! Spins on Aeron poll in a dedicated background thread and feeds messages
//! into wingfoil via a channel.  The graph thread is free to do other work;
//! downstream nodes tick whenever the channel delivers a batch.
//!
//! Use this when you want Aeron on a non-critical path and don't want it to
//! consume a graph-thread CPU core.  For the primary ultra-low-latency pattern
//! use [`AeronSpinSubNode`](super::sub_spin::AeronSpinSubNode) instead.

use crate::adapters::aeron::transport::AeronSubscriberBackend;
use crate::channel::Message;
use crate::nodes::ReceiverStream;
use crate::{
    Burst, Element, GraphState, IntoStream, MutableNode, Stream, StreamPeekRef, UpStreams,
};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tinyvec::TinyVec;

// ---------------------------------------------------------------------------
// Wrapper node
// ---------------------------------------------------------------------------

/// Wraps [`ReceiverStream`] to add a cooperative stop signal for the
/// background polling thread.  When the graph calls `stop()` this node sets
/// the flag before delegating to `ReceiverStream::stop()`, so the thread
/// exits its loop and `join()` returns promptly instead of blocking forever.
struct ThreadedAeronNode<T: Element + Send> {
    inner: ReceiverStream<T>,
    stop_flag: Arc<AtomicBool>,
}

impl<T: Element + Send> MutableNode for ThreadedAeronNode<T> {
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
        // Signal the polling thread to exit its loop before ReceiverStream
        // calls join(), so we don't block forever on a live Aeron subscription.
        self.stop_flag.store(true, Ordering::Relaxed);
        // Ignore join errors — any real error was already propagated via
        // ReceiverStream's deferred-error mechanism during cycle().
        self.inner.stop(state).ok();
        Ok(())
    }

    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.inner.teardown(state)
    }
}

impl<T: Element + Send> StreamPeekRef<TinyVec<[T; 1]>> for ThreadedAeronNode<T> {
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        self.inner.peek_ref()
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build a threaded Aeron subscriber stream.
///
/// The background thread owns `backend` and `parser` exclusively.  It spins
/// on `backend.poll()` continuously; when fragments arrive they are sent
/// through the channel.  The node batches whatever arrived between
/// graph cycles into a `Burst<T>`.
pub(crate) fn build<T, F, B>(backend: B, parser: F) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    F: FnMut(&[u8]) -> Option<T> + Send + 'static,
    B: AeronSubscriberBackend,
{
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_thread = stop_flag.clone();

    // `ReceiverStream::new` requires `Fn` (not `FnOnce`), so we store the
    // owned state in a `Mutex<Option<…>>` and extract it on first (and only)
    // invocation of the closure.
    let state = Mutex::new(Some((backend, parser)));
    let inner = ReceiverStream::new(
        move |sender| {
            let (mut backend, mut parser) = state
                .lock()
                .unwrap()
                .take()
                .expect("threaded aeron closure called more than once");
            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    return Ok(());
                }
                backend.poll(&mut |fragment| {
                    if let Some(v) = parser(fragment) {
                        // Ignore send errors — graph has stopped, thread exits on next loop.
                        let _ = sender.send_message(Message::RealtimeValue(v));
                    }
                })?;
                // Yield when idle to avoid appearing as 100% CPU on profilers.
                std::thread::yield_now();
            }
        },
        true, // assert_realtime: Aeron makes no sense in historical mode
    );

    ThreadedAeronNode { inner, stop_flag }.into_stream()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::aeron::transport::MockSubscriber;
    use crate::{NodeOperators, RunFor, RunMode, StreamOperators};
    use std::time::Duration;

    fn i64_parser(bytes: &[u8]) -> Option<i64> {
        bytes.try_into().ok().map(i64::from_le_bytes)
    }

    /// A backend that delivers one batch of messages then signals the thread
    /// to exit by returning an error.
    struct FiniteSubscriber {
        messages: Vec<Vec<u8>>,
        sent: bool,
    }

    impl FiniteSubscriber {
        fn new(messages: Vec<Vec<u8>>) -> Self {
            Self {
                messages,
                sent: false,
            }
        }
    }

    impl AeronSubscriberBackend for FiniteSubscriber {
        fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
            if self.sent {
                anyhow::bail!("end of test stream");
            }
            self.sent = true;
            let count = self.messages.len();
            for msg in &self.messages {
                handler(msg);
            }
            Ok(count)
        }
    }

    #[test]
    fn threaded_subscriber_delivers_messages() {
        let msgs = vec![
            1i64.to_le_bytes().to_vec(),
            2i64.to_le_bytes().to_vec(),
            3i64.to_le_bytes().to_vec(),
        ];
        let backend = FiniteSubscriber::new(msgs);
        let stream = build(backend, i64_parser);

        let collected = stream.collect();
        // The background thread exits via error; graph propagates it.
        let result = collected
            .clone()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)));
        assert!(result.is_err(), "expected thread exit error to propagate");

        // Despite the error, the values we sent should have been collected.
        let values: Vec<i64> = collected
            .peek_value()
            .into_iter()
            .flat_map(|burst| burst.value)
            .collect();
        assert_eq!(values, vec![1i64, 2, 3]);
    }

    #[test]
    fn threaded_subscriber_historical_mode_fails() {
        use crate::NanoTime;
        let backend = MockSubscriber::from_messages(vec![]);
        let stream = build(backend, i64_parser);
        let result = stream
            .as_node()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));
        assert!(
            result.is_err(),
            "historical mode should fail for threaded Aeron subscriber"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("real-time"),
            "expected real-time error, got: {msg}"
        );
    }
}
