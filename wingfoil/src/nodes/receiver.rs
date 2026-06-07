use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use crate::{
    ChannelReceiverStream, Element, MutableNode, ReadyNotifier, RunMode, StreamPeekRef, UpStreams,
    channel::{ChannelSender, channel_pair},
};
use tinyvec::TinyVec;

enum State<T: Element + Send> {
    Func(Box<dyn Fn(ChannelSender<T>, Arc<AtomicBool>) -> anyhow::Result<()> + Send + 'static>),
    JoinHandle(JoinHandle<anyhow::Result<()>>),
    /// The producer thread completed cleanly after signalling end-of-stream.
    Done,
    Empty,
}

impl<T: Element + Send> State<T> {
    pub fn start(&mut self, channel_sender: ChannelSender<T>, stop: Arc<AtomicBool>) {
        if let State::Func(f) = std::mem::replace(self, State::Empty) {
            let handle = thread::spawn(move || f(channel_sender, stop));
            *self = State::JoinHandle(handle);
        }
    }

    /// Check whether the producer thread is still healthy.
    ///
    /// `finished` reports whether the channel has drained a
    /// [`Message::EndOfStream`](crate::channel::Message::EndOfStream): a thread
    /// that returns `Ok(())` after signalling end-of-stream has shut down
    /// cleanly and is not treated as an error.
    pub fn check_running(&mut self, finished: bool) -> anyhow::Result<()> {
        match self {
            State::JoinHandle(handle) if handle.is_finished() => {
                let State::JoinHandle(handle) = std::mem::replace(self, State::Empty) else {
                    unreachable!("state matched JoinHandle on the previous line");
                };
                match handle.join() {
                    Err(e) => Err(anyhow::anyhow!("Receiver thread panicked: {e:?}")),
                    Ok(Err(e)) => Err(e),
                    // A producer that signalled end-of-stream before returning
                    // has shut down cleanly — not an unexpected exit.
                    Ok(Ok(())) if finished => {
                        *self = State::Done;
                        Ok(())
                    }
                    Ok(Ok(())) => Err(anyhow::anyhow!("Receiver thread exited unexpectedly")),
                }
            }
            State::JoinHandle(_) | State::Done => Ok(()),
            State::Func(_) | State::Empty => Err(anyhow::anyhow!("Receiver thread not running")),
        }
    }

    pub fn stop(&mut self) -> anyhow::Result<()> {
        match std::mem::replace(self, State::Empty) {
            State::JoinHandle(handle) => handle
                .join()
                .map_err(|e| anyhow::anyhow!("Thread panicked: {e:?}"))?,
            _ => Ok(()),
        }
    }
}

pub(crate) struct ReceiverStream<T: Element + Send> {
    inner: ChannelReceiverStream<T>,
    sender: Option<ChannelSender<T>>,
    state: State<T>,
    stop: Arc<AtomicBool>,
    assert_realtime: bool,
    /// Error from the background thread, deferred until the channel is drained.
    pending_err: Option<anyhow::Error>,
    /// Self-notifier used to schedule one extra graph cycle after draining the
    /// channel when the thread exits, so the pending error is eventually delivered.
    notifier: Option<ReadyNotifier>,
}

impl<T: Element + Send> MutableNode for ReceiverStream<T> {
    fn upstreams(&self) -> UpStreams {
        self.inner.upstreams()
    }

    fn cycle(&mut self, state: &mut crate::GraphState) -> anyhow::Result<bool> {
        // Deliver a previously-deferred thread error only once the channel is empty.
        if let Some(e) = self.pending_err.take() {
            return Err(e);
        }

        // Drain the channel first, then check thread state. This avoids a race
        // where the thread exits after check_running but before we drain: if we
        // checked first, a still-running thread would skip error handling, and
        // nothing would wake the graph once the sender drops.
        let cycle_result = self.inner.cycle(state)?;
        if let Err(thread_err) = self.state.check_running(self.inner.finished()) {
            self.pending_err = Some(thread_err);
            // Self-notify: schedule one more graph cycle to propagate the error.
            if let Some(notifier) = &self.notifier {
                let _ = notifier.notify();
            }
        }
        Ok(cycle_result)
    }

    fn setup(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        let mut sender = self
            .sender
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing sender"))?;
        if state.run_mode() == RunMode::RealTime {
            let notifier = state.ready_notifier();
            sender.set_notifier(notifier.clone());
            self.notifier = Some(notifier);
        }
        self.state.start(sender, self.stop.clone());
        self.inner.setup(state)
    }

    fn start(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        if self.assert_realtime && state.run_mode() != RunMode::RealTime {
            anyhow::bail!("ReceiverStream only supports real-time mode");
        }
        self.inner.start(state)
    }

    fn stop(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.state.stop()?;
        self.inner.stop(state)
    }

    fn teardown(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        self.inner.teardown(state)
    }
}

impl<T: Element + Send> StreamPeekRef<TinyVec<[T; 1]>> for ReceiverStream<T> {
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        self.inner.peek_ref()
    }
}

impl<T: Element + Send> ReceiverStream<T> {
    pub(crate) fn new(
        f: impl Fn(ChannelSender<T>, Arc<AtomicBool>) -> anyhow::Result<()> + Send + 'static,
        assert_realtime: bool,
    ) -> Self {
        let (sender, receiver) = channel_pair(None);
        let inner = ChannelReceiverStream::new(receiver, None, None);
        let sender = Some(sender);
        let stop = Arc::new(AtomicBool::new(false));
        let state = State::Func(Box::new(f));
        Self {
            inner,
            sender,
            state,
            stop,
            assert_realtime,
            pending_err: None,
            notifier: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Message;
    use crate::nodes::NodeOperators;
    use crate::{IntoStream, RunFor, RunMode};
    use std::time::Duration;

    /// Spawn a producer that runs `f`, then spin until its thread has exited so
    /// `check_running` exercises the just-finished branch deterministically.
    fn finished_state(f: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> State<u64> {
        let state = State::JoinHandle(thread::spawn(f));
        while !matches!(&state, State::JoinHandle(h) if h.is_finished()) {
            std::thread::yield_now();
        }
        state
    }

    /// A producer that returns `Ok(())` after signalling end-of-stream
    /// (`finished == true`) has shut down cleanly — not an error. Subsequent
    /// checks stay `Ok` because the state transitions to `Done`.
    #[test]
    fn clean_exit_after_end_of_stream_is_ok() {
        let mut state = finished_state(|| Ok(()));
        assert!(state.check_running(true).is_ok());
        assert!(matches!(state, State::Done));
        assert!(state.check_running(true).is_ok());
    }

    /// A producer that returns `Ok(())` without signalling end-of-stream has
    /// exited unexpectedly and must surface as an error.
    #[test]
    fn silent_exit_without_end_of_stream_is_error() {
        let mut state = finished_state(|| Ok(()));
        assert!(state.check_running(false).is_err());
    }

    /// An error returned by the producer thread propagates unchanged.
    #[test]
    fn thread_error_propagates() {
        let mut state = finished_state(|| Err(anyhow::anyhow!("boom")));
        let err = state.check_running(true).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    /// End-to-end: a producer that emits a value, signals end-of-stream, then
    /// returns `Ok(())` must run cleanly (regression: a ZMQ subscriber whose
    /// publisher disconnects mid-run).
    #[test]
    fn clean_end_of_stream_does_not_error_the_graph() {
        let stream = ReceiverStream::new(
            |sender, _stop| {
                sender.send_message(Message::RealtimeValue(1u64))?;
                sender.send_message(Message::EndOfStream)?;
                Ok(())
            },
            true,
        )
        .into_stream();
        stream
            .count()
            .run(
                RunMode::RealTime,
                RunFor::Duration(Duration::from_millis(100)),
            )
            .expect("clean end-of-stream should not error");
    }
}
