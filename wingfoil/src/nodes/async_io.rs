use crate::channel::{ChannelReceiver, ChannelSender, Message, ReceiverMessageSource, channel_pair};
use crate::nodes::channel::ReceiverStream;
use crate::*;

use futures::stream::StreamExt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use tinyvec::TinyVec;
use anyhow::anyhow;

/// A convenience alias for [`futures::Stream`] with items of type `(NanoTime, T)`.
/// used by [StreamOperators::consume_async].
pub trait FutStream<T>: futures::Stream<Item = (NanoTime, T)> + Send {}

impl<STRM, T> FutStream<T> for STRM where STRM: futures::Stream<Item = (NanoTime, T)> + Send {}

type ConsumerFunc<T, FUT> = Box<dyn FnOnce(Pin<Box<dyn FutStream<T>>>) -> FUT + Send>;

pub(crate) struct AsyncConsumerNode<T, FUT>
where
    T: Element + Send,
    FUT: Future<Output = ()> + Send + 'static,
{
    source: Rc<dyn Stream<T>>,
    sender: ChannelSender<T>,
    func: Option<ConsumerFunc<T, FUT>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    rx: Option<ChannelReceiver<T>>,
}

impl<T, FUT> AsyncConsumerNode<T, FUT>
where
    T: Element + Send,
    FUT: Future<Output = ()> + Send + 'static,
{
    pub fn new(source: Rc<dyn Stream<T>>, func: ConsumerFunc<T, FUT>) -> Self {
        let (sender, receiver) = channel_pair(None);
        let rx = Some(receiver);
        let handle = None;

        let func = Some(func);
        Self {
            source,
            sender,
            func,
            handle,
            rx,
        }
    }
}

impl<T, FUT> MutableNode for AsyncConsumerNode<T, FUT>
where
    T: Element + Send,
    FUT: Future<Output = ()> + Send + 'static,
{
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        let res = self.sender.send(state, self.source.peek_value());
        if  res.is_err() {
            state.terminate(res.map_err(|e| anyhow!(e)));
        }
        true
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }

    fn setup(&mut self, state: &mut GraphState) {
        let run_mode = state.run_mode();
        let run_for = state.run_for();
        let rx = self.rx.take().unwrap();
        let func = self.func.take().unwrap();
        let f = async move {
            let src = rx
                .to_boxed_message_stream()
                .limit(run_mode, run_for)
                .to_stream();
            let fut = func(Box::pin(src));
            fut.await;
        };
        let handle = state.tokio_runtime().spawn(f);
        self.handle = Some(handle);
    }

    fn stop(&mut self, state : &mut GraphState) {
        let res = self.sender.close();
        if res.is_err() {
            state.terminate(res.map_err(|e| anyhow!(e)));
        }
    }

    fn teardown(&mut self, state: &mut GraphState) {
        let handle = self.handle.take().unwrap();
        state.tokio_runtime().block_on(handle).unwrap();
    }
}

struct AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = (NanoTime, T)> + Send + 'static,
    FUT: Future<Output = S> + Send + 'static,
    FUNC: FnOnce() -> FUT + Send + 'static,
{
    func: Option<FUNC>,
    receiver_stream: ReceiverStream<T>,
    handle: Option<tokio::task::JoinHandle<()>>,
    sender: Option<ChannelSender<T>>,
}

impl<T, S, FUT, FUNC> AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = (NanoTime, T)> + Send + 'static,
    FUT: Future<Output = S> + Send + 'static,
    FUNC: FnOnce() -> FUT + Send + 'static,
{
    pub fn new(func: FUNC) -> Self {
        let (sender, receiver) = channel_pair(None);
        let receiver_stream = ReceiverStream::new(receiver, None, None);
        let handle = None;
        let func = Some(func);
        let sender = Some(sender);
        Self {
            receiver_stream,
            func,
            handle,
            sender,
        }
    }
}

impl<T, S, FUT, FUNC> MutableNode for AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = (NanoTime, T)> + Send + 'static,
    FUT: Future<Output = S> + Send + 'static,
    FUNC: FnOnce() -> FUT + Send + 'static,
{
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        self.receiver_stream.cycle(state)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }

    fn setup(&mut self, state: &mut GraphState) {
        let run_mode = state.run_mode();
        let run_for = state.run_for();
        let mut sender = self.sender.take().unwrap();
        match run_mode {
            RunMode::HistoricalFrom(_) => {}
            RunMode::RealTime => sender.set_notifier(state.ready_notifier()),
        };
        let mut sender = sender.into_async();
        let func = self.func.take().unwrap();
        let fut = async move {
            let source = func()
                .await
                .to_message_stream(run_mode)
                .limit(run_mode, run_for);
            let mut source = Box::pin(source);
            while let Some(message) = source.next().await {
                sender.send_message(message).await;
            }
            sender.close().await;
        };
        let handle = state.tokio_runtime().spawn(fut);
        self.handle = Some(handle);
        self.receiver_stream.setup(state);
    }

    fn teardown(&mut self, state: &mut GraphState) {
        self.receiver_stream.teardown(state);
        let handle = self.handle.take().unwrap();
        state.tokio_runtime().block_on(handle).unwrap();
    }
}

impl<T, S, FUT, FUNC> StreamPeekRef<TinyVec<[T; 1]>> for AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = (NanoTime, T)> + Send + 'static,
    FUT: Future<Output = S> + Send + 'static,
    FUNC: FnOnce() -> FUT + Send + 'static,
{
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        self.receiver_stream.peek_ref()
    }
}

/// Create a [Stream] from futures::Stream
pub fn produce_async<T, S, FUT, FUNC>(func: FUNC) -> Rc<dyn Stream<TinyVec<[T; 1]>>>
where
    T: Element + Send,
    S: futures::Stream<Item = (NanoTime, T)> + Send + 'static,
    FUT: Future<Output = S> + Send + 'static,
    FUNC: FnOnce() -> FUT + Send + 'static,
{
    AsyncProducerStream::new(func).into_stream()
}

trait StreamMessageSource<T: Element + Send> {
    fn to_message_stream(self, run_mode: RunMode) -> impl futures::Stream<Item = Message<T>>;
}

impl<T, STRM> StreamMessageSource<T> for STRM
where
    STRM: futures::Stream<Item = (NanoTime, T)>,
    T: Element + Send,
{
    fn to_message_stream(self, run_mode: RunMode) -> impl futures::Stream<Item = Message<T>> {
        async_stream::stream! {
            let mut source = Box::pin(self);
            while let Some((time, value)) = source.next().await {
                match run_mode {
                    RunMode::RealTime => {
                        yield Message::RealtimeValue(value);
                    }
                    RunMode::HistoricalFrom(_) => {
                        yield Message::HistoricalValue(ValueAt::new(value, time));
                    },
                }
            }
            yield Message::EndOfStream;
        }
    }
}

trait MessageStream<T: Element + Send> {
    fn limit(self, run_mode: RunMode, run_for: RunFor) -> impl futures::Stream<Item = Message<T>>;

    fn to_stream(self) -> impl futures::Stream<Item = (NanoTime, T)>;
}

impl<T, STRM> MessageStream<T> for STRM
where
    STRM: futures::Stream<Item = Message<T>>,
    T: Element + Send,
{
    fn limit(self, run_mode: RunMode, run_for: RunFor) -> impl futures::Stream<Item = Message<T>> {
        async_stream::stream! {
            let time0 = run_mode.start_time();
            let mut time = NanoTime::ZERO;
            let mut elapsed:  NanoTime;
            let mut cycle = 0;
            let mut source = Box::pin(self);
            let mut finished = false;
            while let Some(message) = source.next().await
            {
                match &message {
                    Message::RealtimeValue(_) => {
                        time = NanoTime::now();
                    }
                    Message::HistoricalValue(value_at) => {
                        time = value_at.time;
                    },
                    Message::CheckPoint(t) => {
                        time = *t;
                    },
                    Message::EndOfStream => {
                        finished = true;
                    },
                }
                elapsed = time - time0;
                yield (message);
                if run_for.done(cycle, elapsed) || finished {
                    break;
                }
                cycle +=1;
            }
        }
    }

    fn to_stream(self) -> impl futures::Stream<Item = (NanoTime, T)> {
        async_stream::stream! {
            let mut source = Box::pin(self);
            while let Some(message) = source.next().await {
                match message {
                    Message::RealtimeValue(value) => {
                        yield (NanoTime::now(), value)
                    }
                    Message::HistoricalValue(value_at) => {
                        yield (value_at.time, value_at.value)
                    },
                    Message::CheckPoint(_) => {},
                    Message::EndOfStream => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::*;
    use futures::StreamExt;
    use std::pin::Pin;
    use std::time::Duration;

    #[test]
    fn async_io_works() {
        let _ = env_logger::try_init();
        let n_runs = 5;
        let period = Duration::from_millis(10);
        let n_periods = 5;
        let run_for = RunFor::Duration(period * n_periods);

        for _ in 0..n_runs {
            for run_mode in [RunMode::RealTime, RunMode::HistoricalFrom(NanoTime::ZERO)] {
                let example_producer = async move || {
                    async_stream::stream! {
                        for i in 0.. {
                            let time = match run_mode {
                                RunMode::HistoricalFrom(time0) => {
                                    // wire up historical source here
                                    time0 + period * i
                                },
                                RunMode::RealTime => {
                                    // wire up real time source here
                                    tokio::time::sleep(period).await; // simulate waiting IO
                                    NanoTime::now()
                                }
                            };
                            yield (time, i * 10);
                        }
                    }
                };

                let example_consumer = async move |mut source: Pin<Box<dyn FutStream<u32>>>| {
                    while let Some((time, value)) = source.next().await {
                        println!("{time:?}, {value:?}");
                    }
                };

                produce_async(example_producer)
                    .collapse()
                    .logged("on-graph", log::Level::Info)
                    .consume_async(Box::new(example_consumer))
                    .run(run_mode, run_for)
                    .unwrap();
            }
        }
    }
}
