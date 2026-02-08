use crate::channel::{
    ChannelReceiver, ChannelSender, Message, ReceiverMessageSource, channel_pair,
};
use crate::nodes::channel::ReceiverStream;
use crate::*;

use futures::stream::StreamExt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

/// Context passed to async producer closures during graph setup.
///
/// This provides the run configuration so producers can adapt their behavior
/// (e.g., derive time ranges for database queries).
#[derive(Clone, Copy, Debug)]
pub struct RunParams {
    pub run_mode: RunMode,
    pub run_for: RunFor,
    pub start_time: NanoTime,
}

impl RunParams {
    /// Compute end time based on run_for.
    ///
    /// Returns `start_time + duration` for `RunFor::Duration`,
    /// `NanoTime::MAX` for `Forever`, or an error for `Cycles`.
    pub fn end_time(&self) -> anyhow::Result<NanoTime> {
        match self.run_for {
            RunFor::Duration(d) => Ok(self.start_time + d),
            RunFor::Forever => Ok(NanoTime::MAX),
            RunFor::Cycles(_) => anyhow::bail!("end_time not available for RunFor::Cycles"),
        }
    }
}

/// A convenience alias for [`futures::Stream`] with items of type `(NanoTime, T)`.
/// used by [StreamOperators::consume_async].
pub trait FutStream<T>: futures::Stream<Item = (NanoTime, T)> + Send {}

impl<STRM, T> FutStream<T> for STRM where STRM: futures::Stream<Item = (NanoTime, T)> + Send {}

type ConsumerFunc<T, FUT> = Box<dyn FnOnce(Pin<Box<dyn FutStream<T>>>) -> FUT + Send>;

pub(crate) struct AsyncConsumerNode<T, FUT>
where
    T: Element + Send,
    FUT: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    source: Rc<dyn Stream<T>>,
    sender: ChannelSender<T>,
    func: Option<ConsumerFunc<T, FUT>>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    rx: Option<ChannelReceiver<T>>,
}

impl<T, FUT> AsyncConsumerNode<T, FUT>
where
    T: Element + Send,
    FUT: Future<Output = anyhow::Result<()>> + Send + 'static,
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
    FUT: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.sender.send(state, self.source.peek_value())?;
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }

    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        let run_mode = state.run_mode();
        let run_for = state.run_for();
        let rx = self
            .rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("rx is already taken"))?;
        let func = self
            .func
            .take()
            .ok_or_else(|| anyhow::anyhow!("func is already taken"))?;
        let f = async move {
            let src = rx
                .to_boxed_message_stream()
                .limit(run_mode, run_for)
                .to_stream();
            let fut = func(Box::pin(src));
            fut.await
        };
        let handle = state.tokio_runtime().spawn(f);
        self.handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        self.sender.close()?;
        Ok(())
    }

    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if let Some(handle) = self.handle.take() {
            state.tokio_runtime().block_on(handle)??;
        }
        Ok(())
    }
}

struct AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
    FUT: Future<Output = anyhow::Result<S>> + Send + 'static,
    FUNC: FnOnce(RunParams) -> FUT + Send + 'static,
{
    func: Option<FUNC>,
    receiver_stream: ReceiverStream<T>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    sender: Option<ChannelSender<T>>,
}

impl<T, S, FUT, FUNC> AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
    FUT: Future<Output = anyhow::Result<S>> + Send + 'static,
    FUNC: FnOnce(RunParams) -> FUT + Send + 'static,
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
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
    FUT: Future<Output = anyhow::Result<S>> + Send + 'static,
    FUNC: FnOnce(RunParams) -> FUT + Send + 'static,
{
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.receiver_stream.cycle(state)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }

    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        let run_mode = state.run_mode();
        let run_for = state.run_for();
        let mut sender = self
            .sender
            .take()
            .ok_or_else(|| anyhow::anyhow!("sender is already taken"))?;

        match run_mode {
            RunMode::HistoricalFrom(_) => {}
            RunMode::RealTime => sender.set_notifier(state.ready_notifier()),
        };
        let mut sender = sender.into_async();
        let func = self
            .func
            .take()
            .ok_or_else(|| anyhow::anyhow!("func is already taken"))?;
        let ctx = RunParams {
            run_mode,
            run_for,
            start_time: state.start_time(),
        };
        let fut = async move {
            let stream = func(ctx).await?;
            let source = stream.to_message_stream(run_mode).limit(run_mode, run_for);
            let mut source = Box::pin(source);
            while let Some(message) = source.next().await {
                sender.send_message(message).await;
            }
            sender.close().await;
            Ok(())
        };
        let handle = state.tokio_runtime().spawn(fut);
        self.handle = Some(handle);
        self.receiver_stream.setup(state)?;
        Ok(())
    }

    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.receiver_stream.teardown(state)?;
        if let Some(handle) = self.handle.take() {
            state.tokio_runtime().block_on(handle)??;
        }
        Ok(())
    }
}

impl<T, S, FUT, FUNC> StreamPeekRef<Burst<T>> for AsyncProducerStream<T, S, FUT, FUNC>
where
    T: Element + Send,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
    FUT: Future<Output = anyhow::Result<S>> + Send + 'static,
    FUNC: FnOnce(RunParams) -> FUT + Send + 'static,
{
    fn peek_ref(&self) -> &Burst<T> {
        self.receiver_stream.peek_ref()
    }
}

/// Create a [Stream] from a fallible async function that produces a futures::Stream.
///
/// The closure receives an [`RunParams`] with `run_mode`, `run_for`, and `start_time`,
/// allowing producers to adapt their behavior (e.g., derive time ranges for queries).
///
/// # Example
/// ```ignore
/// produce_async(|ctx| async move {
///     let start = ctx.start_time;
///     let end = ctx.end_time();
///     Ok(async_stream::stream! {
///         // Use start, end in async code...
///     })
/// })
/// ```
pub fn produce_async<T, S, FUT, FUNC>(func: FUNC) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
    FUT: Future<Output = anyhow::Result<S>> + Send + 'static,
    FUNC: FnOnce(RunParams) -> FUT + Send + 'static,
{
    AsyncProducerStream::new(func).into_stream()
}

trait StreamMessageSource<T: Element + Send> {
    fn to_message_stream(self, run_mode: RunMode) -> impl futures::Stream<Item = Message<T>>;
}

impl<T, STRM> StreamMessageSource<T> for STRM
where
    STRM: futures::Stream<Item = anyhow::Result<(NanoTime, T)>>,
    T: Element + Send,
{
    fn to_message_stream(self, run_mode: RunMode) -> impl futures::Stream<Item = Message<T>> {
        async_stream::stream! {
            let mut source = Box::pin(self);
            while let Some(result) = source.next().await {
                match result {
                    Ok((time, value)) => match run_mode {
                        RunMode::RealTime => yield Message::RealtimeValue(value),
                        RunMode::HistoricalFrom(_) => yield Message::HistoricalValue(ValueAt::new(value, time)),
                    },
                    Err(e) => {
                        yield Message::Error(std::sync::Arc::new(e));
                    }
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
            let mut time = time0;
            let mut elapsed: NanoTime;
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
                    Message::HistoricalBatch(batch) => {
                        // Update time to earliest timestamp in batch
                        if let Some(value_at) = batch.first() {
                            time = value_at.time;
                        }
                    }
                    Message::EndOfStream => {
                        finished = true;
                    },
                    Message::Error(_) => {
                        // Error messages are passed through
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
                    Message::HistoricalBatch(batch) => {
                        // Convert to Vec to own the data and avoid holding reference across await
                        let batch_vec = batch.into_vec();
                        for value_at in batch_vec {
                            yield (value_at.time, value_at.value)
                        }
                    }
                    Message::CheckPoint(_) => {},
                    Message::EndOfStream => {},
                    Message::Error(_) => {},
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
    fn produce_async_mid_stream_error() {
        let _ = env_logger::try_init();
        let period = Duration::from_millis(10);

        let producer = move |ctx: RunParams| async move {
            Ok(async_stream::stream! {
                for i in 0..3u32 {
                    let time = ctx.start_time + period * i;
                    yield Ok((time, i));
                }
                yield Err(anyhow::anyhow!("something went wrong"));
            })
        };

        let result = produce_async(producer)
            .collapse()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);

        let err = result.unwrap_err();
        let root = err.root_cause().to_string();
        assert!(
            root.contains("something went wrong"),
            "expected error message, got: {root}"
        );
    }

    #[test]
    fn async_io_works() {
        let _ = env_logger::try_init();
        let n_runs = 5;
        let period = Duration::from_millis(10);
        let n_periods = 5;
        let run_for = RunFor::Duration(period * n_periods);

        for _ in 0..n_runs {
            for run_mode in [RunMode::RealTime, RunMode::HistoricalFrom(NanoTime::ZERO)] {
                let example_producer = move |ctx: RunParams| async move {
                    Ok(async_stream::stream! {
                        for i in 0.. {
                            let time = match ctx.run_mode {
                                RunMode::HistoricalFrom(_) => {
                                    // wire up historical source here
                                    ctx.start_time + period * i
                                },
                                RunMode::RealTime => {
                                    // wire up real time source here
                                    tokio::time::sleep(period).await; // simulate waiting IO
                                    NanoTime::now()
                                }
                            };
                            yield Ok((time, i * 10));
                        }
                    })
                };

                let example_consumer = async move |mut source: Pin<Box<dyn FutStream<u32>>>| {
                    while let Some((time, value)) = source.next().await {
                        println!("{time:?}, {value:?}");
                    }
                    Ok(())
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
