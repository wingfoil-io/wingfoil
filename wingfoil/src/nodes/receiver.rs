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
    Empty,
}

impl<T: Element + Send> State<T> {
    pub fn start(&mut self, channel_sender: ChannelSender<T>, stop: Arc<AtomicBool>) {
        if let State::Func(f) = std::mem::replace(self, State::Empty) {
            let handle = thread::spawn(move || f(channel_sender, stop));
            *self = State::JoinHandle(handle);
        }
    }

    pub fn check_running(&mut self) -> anyhow::Result<()> {
        match &*self {
            State::JoinHandle(handle) if handle.is_finished() => {
                if let State::JoinHandle(handle) = std::mem::replace(self, State::Empty) {
                    return match handle.join() {
                        Err(e) => Err(anyhow::anyhow!("Receiver thread panicked: {e:?}")),
                        Ok(Err(e)) => Err(e),
                        Ok(Ok(())) => Err(anyhow::anyhow!("Receiver thread exited unexpectedly")),
                    };
                }
                Ok(())
            }
            State::JoinHandle(_) => Ok(()),
            _ => Err(anyhow::anyhow!("Receiver thread not running")),
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
        if let Err(thread_err) = self.state.check_running() {
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
