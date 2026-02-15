use std::thread::{self, JoinHandle};

use crate::{
    ChannelReceiverStream, Element, MutableNode, RunMode, StreamPeekRef,
    channel::{ChannelSender, channel_pair},
};
use tinyvec::TinyVec;

enum State<T: Element + Send> {
    Func(Box<dyn Fn(ChannelSender<T>) -> anyhow::Result<()> + Send + 'static>),
    JoinHandle(JoinHandle<anyhow::Result<()>>),
    Empty,
}

impl<T: Element + Send> State<T> {
    pub fn start(&mut self, channel_sender: ChannelSender<T>) {
        if let State::Func(f) = std::mem::replace(self, State::Empty) {
            let handle = thread::spawn(move || f(channel_sender));
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
    assert_realtime: bool,
}

impl<T: Element + Send> MutableNode for ReceiverStream<T> {
    fn cycle(&mut self, state: &mut crate::GraphState) -> anyhow::Result<bool> {
        self.state.check_running()?;
        self.inner.cycle(state)
    }

    fn upstreams(&self) -> crate::UpStreams {
        self.inner.upstreams()
    }

    fn setup(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        let mut sender = self
            .sender
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing sender"))?;
        if state.run_mode() == RunMode::RealTime {
            sender.set_notifier(state.ready_notifier());
        }
        self.state.start(sender);
        self.inner.setup(state)
    }

    fn start(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
        if self.assert_realtime && state.run_mode() != RunMode::RealTime {
            anyhow::bail!("ReceiverStream only supports real-time mode");
        }
        self.inner.start(state)
    }

    fn stop(&mut self, state: &mut crate::GraphState) -> anyhow::Result<()> {
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
        f: impl Fn(ChannelSender<T>) -> anyhow::Result<()> + Send + 'static,
        assert_realtime: bool,
    ) -> Self {
        let (sender, receiver) = channel_pair(None);
        let inner = ChannelReceiverStream::new(receiver, None, None);
        let sender = Some(sender);
        let state = State::Func(Box::new(f));
        Self {
            inner,
            sender,
            state,
            assert_realtime,
        }
    }
}
