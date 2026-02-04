use crate::nodes::channel::ReceiverStream;
use crate::*;
use channel::{ChannelSender, channel_pair};
use nodes::channel::ChannelOperators;

use std::cell::OnceCell;
use std::cmp::Eq;
use std::hash::Hash;
use std::mem;
use std::rc::Rc;
use std::time::Duration;
use std::{thread, vec};
use tinyvec::TinyVec;
use std::fmt::Debug;

#[derive(Debug, Default)]
enum GraphProducerStreamState<'a, T, FUNC>
where
    T: Send + 'static,
    FUNC: FnOnce() -> Rc<dyn Stream<'a, T> + 'a> + Send + 'a,
{
    Func(FUNC),
    Handle(thread::JoinHandle<()>),
    #[default]
    Empty,
    _Phantom(std::marker::PhantomData<&'a T>),
}

struct GraphProducerStream<'a, T, FUNC>
where
    T: Send + 'static + Default,
    FUNC: FnOnce() -> Rc<dyn Stream<'a, T> + 'a> + Send + 'a,
{
    receiver_stream: OnceCell<ReceiverStream<'a, T>>,
    state: GraphProducerStreamState<'a, T, FUNC>,
}

impl<'a, T, FUNC> GraphProducerStream<'a, T, FUNC>
where
    T: Send + 'static + Default,
    FUNC: FnOnce() -> Rc<dyn Stream<'a, T> + 'a> + Send + 'a,
{
    fn new(func: FUNC) -> Self {
        let state = GraphProducerStreamState::Func(func);
        let receiver = OnceCell::new();
        Self {
            receiver_stream: receiver,
            state,
        }
    }
}

impl<'a, T, FUNC> MutableNode<'a> for GraphProducerStream<'a, T, FUNC>
where
    T: Debug + Clone + Send + 'static + Default,
    FUNC: FnOnce() -> Rc<dyn Stream<'a, T> + 'a> + Send + 'static,
{
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::none()
    }

    fn cycle(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.receiver_stream.get_mut().unwrap().cycle(graph_state)
    }

    fn setup(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<()> {
        let state = mem::take(&mut self.state);
        match state {
            GraphProducerStreamState::Func(func) => {
                let run_for = graph_state.run_for();
                let notifier = match graph_state.run_mode() {
                    RunMode::HistoricalFrom(_) => None,
                    RunMode::RealTime => Some(graph_state.ready_notifier()),
                };
                let (sender, receiver) = channel_pair(notifier);
                let mut receiver_stream = ReceiverStream::new(receiver, None, None);
                receiver_stream.setup(graph_state)?;
                self.receiver_stream.set(receiver_stream).unwrap();
                let tokio_runtime = graph_state.tokio_runtime();
                let start_time = graph_state.start_time();
                let run_mode = graph_state.run_mode();
                let task = move || {
                    let node = func().send(sender, None);
                    let mut graph =
                        Graph::new_with(vec![node], tokio_runtime, run_mode, run_for, start_time);
                    graph.run().unwrap();
                };

                let handle = thread::spawn(task);
                self.state = GraphProducerStreamState::Handle(handle);
            }
            _ => panic!(),
        }
        Ok(())
    }

    fn teardown(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.receiver_stream
            .get_mut()
            .unwrap()
            .teardown(graph_state)?;
        let state = mem::take(&mut self.state);
        match state {
            GraphProducerStreamState::Handle(handle) => {
                handle.join().unwrap();
                self.state = GraphProducerStreamState::Empty;
            }
            _ => anyhow::bail!("unexpected state"),
        }
        Ok(())
    }
}

impl<'a, T, FUNC> StreamPeekRef<'a, TinyVec<[T; 1]>> for GraphProducerStream<'a, T, FUNC>
where
    T: Debug + Clone + Send + Hash + Eq + 'static + Default,
    FUNC: FnOnce() -> Rc<dyn Stream<'a, T> + 'a> + Send + 'static,
{
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        self.receiver_stream.get().unwrap().peek_ref()
    }
}

/// Creates a [Stream] emitting values on this thread
/// but produced on a worker thread.
pub fn producer<'a, T: Debug + Clone + Default + Send + Hash + Eq + 'static>(
    func: impl FnOnce() -> Rc<dyn Stream<'a, T> + 'a> + Send + 'static,
) -> Rc<dyn Stream<'a, TinyVec<[T; 1]>> + 'a> {
    GraphProducerStream::new(func).into_stream()
}

#[derive(Debug, Default)]
enum GraphMapStreamState<'a, FUNC, IN, OUT>
where
    IN: Send + 'static + Default,
    OUT: Send + 'static + Default,
    FUNC: FnOnce(Rc<dyn Stream<'a, TinyVec<[IN; 1]>> + 'a>) -> Rc<dyn Stream<'a, OUT> + 'a> + Send + 'a,
{
    Func(FUNC, ChannelSender<OUT>),
    Handle(thread::JoinHandle<()>),
    #[default]
    Empty,
    #[allow(dead_code)]
    _Phantom(IN, OUT, std::marker::PhantomData<&'a ()>),
}

pub(crate) struct GraphMapStream<'a, FUNC, IN, OUT>
where
    IN: Send + 'static + Default,
    OUT: Send + 'static + Default,
    FUNC: FnOnce(Rc<dyn Stream<'a, TinyVec<[IN; 1]>> + 'a>) -> Rc<dyn Stream<'a, OUT> + 'a> + Send + 'a,
{
    source: Rc<dyn Stream<'a, IN> + 'a>,
    sender: OnceCell<ChannelSender<IN>>,
    receiver_stream: ReceiverStream<'a, OUT>,
    state: GraphMapStreamState<'a, FUNC, IN, OUT>,
}

impl<'a, IN, OUT, FUNC> GraphMapStream<'a, FUNC, IN, OUT>
where
    IN: Send + 'static + Default,
    OUT: Send + 'static + Default,
    FUNC: FnOnce(Rc<dyn Stream<'a, TinyVec<[IN; 1]>> + 'a>) -> Rc<dyn Stream<'a, OUT> + 'a> + Send + 'a,
{
    pub fn new(source: Rc<dyn Stream<'a, IN> + 'a>, func: FUNC) -> Self {
        let trigger = Some(source.clone().as_node());
        let (sender_out, receiver_out) = channel_pair(None);
        //let receiver_out = ChannelReceiver::new(rx_out);
        let receiver_stream = ReceiverStream::new(receiver_out, trigger, None);
        let state = GraphMapStreamState::Func(func, sender_out);
        let sender = OnceCell::new();
        GraphMapStream {
            source,
            sender,
            receiver_stream,
            state,
        }
    }
}

impl<'a, IN, OUT, FUNC> MutableNode<'a> for GraphMapStream<'a, FUNC, IN, OUT>
where
    IN: Debug + Clone + Send + 'static + Default,
    OUT: Debug + Clone + Send + 'static + Default,
    FUNC: FnOnce(Rc<dyn Stream<'a, TinyVec<[IN; 1]>> + 'a>) -> Rc<dyn Stream<'a, OUT> + 'a> + Send + 'static,
{
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }

    fn cycle(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        if graph_state.ticked(self.source.clone().as_node()) {
            self.sender
                .get_mut()
                .unwrap()
                .send(graph_state, self.source.peek_value())?;
        }
        self.receiver_stream.cycle(graph_state)
    }

    fn setup(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<()> {
        let state = mem::take(&mut self.state);
        match state {
            GraphMapStreamState::Func(func, sender_out) => {
                let mut sender_out = sender_out;
                let run_mode = graph_state.run_mode();
                match run_mode {
                    RunMode::RealTime => {
                        sender_out.set_notifier(graph_state.ready_notifier());
                    }
                    RunMode::HistoricalFrom(_) => {}
                };
                let (tx_notif, rx_notif) = kanal::unbounded();
                let tx_notif = match run_mode {
                    RunMode::RealTime => Some(tx_notif),
                    RunMode::HistoricalFrom(_) => None,
                };
                let run_mode = graph_state.run_mode();
                let run_for = graph_state.run_for();
                let tokio_runtime = graph_state.tokio_runtime();
                let start_time = graph_state.start_time();
                let (mut sender_in, receiver_in) = channel_pair(None);
                let task = move || {
                    let src = ReceiverStream::new(receiver_in, None, tx_notif).into_stream();
                    let node = func(src.clone()).send(sender_out, Some(src.as_node()));
                    let mut graph =
                        Graph::new_with(vec![node], tokio_runtime, run_mode, run_for, start_time);
                    graph.run().unwrap();
                };
                let handle = thread::spawn(task);
                self.state = GraphMapStreamState::Handle(handle);
                match run_mode {
                    RunMode::HistoricalFrom(_) => {}
                    RunMode::RealTime => {
                        let timeout = Duration::from_millis(100);
                        let notifier = rx_notif.recv_timeout(timeout).unwrap();
                        sender_in.set_notifier(notifier);
                    }
                };
                self.sender.set(sender_in).unwrap();
            }
            _ => anyhow::bail!("Invalid state"),
        }
        self.receiver_stream.setup(graph_state)
    }

    fn stop(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<()> {
        if let Some(sender) = self.sender.get_mut() {
            sender.close()?;
        }
        Ok(())
    }

    fn teardown(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.receiver_stream.teardown(graph_state)?;
        let state = mem::take(&mut self.state);
        match state {
            GraphMapStreamState::Handle(handle) => {
                handle.join().unwrap();
                self.state = GraphMapStreamState::Empty;
            }
            _ => anyhow::bail!("Invalid state"),
        }
        Ok(())
    }
}

impl<'a, IN, OUT, FUNC> StreamPeekRef<'a, TinyVec<[OUT; 1]>> for GraphMapStream<'a, FUNC, IN, OUT>
where
    IN: Debug + Clone + Send + 'static + Default,
    OUT: Debug + Clone + Send + 'static + Default,
    FUNC: FnOnce(Rc<dyn Stream<'a, TinyVec<[IN; 1]>> + 'a>) -> Rc<dyn Stream<'a, OUT> + 'a> + Send + 'static,
{
    fn peek_ref(&self) -> &TinyVec<[OUT; 1]> {
        self.receiver_stream.peek_ref()
    }
}

#[cfg(test)]
mod tests {

    use crate::*;
    use std::panic::catch_unwind;
    use std::rc::Rc;
    use std::{thread, time::Duration};
    use tinyvec::TinyVec;

    #[test]
    fn graph_node_works() {
        //_ = env_logger::try_init();
        let n_runs = 1;
        let n_ticks = 6;
        let half_period = Duration::from_millis(5);
        let period = half_period * 2;
        let run_for = RunFor::Duration(period * n_ticks);

        let delays = [
            Duration::ZERO,
            //half_period,
            //period,
        ];
        let run_modes = [RunMode::HistoricalFrom(NanoTime::ZERO), RunMode::RealTime];

        let true_false = vec![false, true];

        for (run_mode, delay, sleep_produce, sleep_map, _) in
            itertools::iproduct!(run_modes, delays, true_false.clone(), true_false, 0..n_runs)
        {
            let label = move || {
                let thread_id = thread::current().id();
                let lbl = format!("{:?}>>", thread_id);
                lbl
            };
            let seq = move || {
                ticker(period)
                    .count()
                    .limit(n_ticks)
                    .map(move |x| {
                        if sleep_produce {
                            std::thread::sleep(period * 2);
                        }
                        x
                    })
                    .logged(label().as_str(), log::Level::Info)
            };
            
            let f = move || {
                let label_inner = label;
                let scale = move |src: Rc<dyn Stream<'static, TinyVec<[TinyVec<[u64; 1]>; 1]>> + 'static>| {
                    src.delay(delay)
                        .map(move |xs| {
                            // xs is TinyVec<[TinyVec<[u64; 1]>; 1]>
                            xs.iter().flatten().map(|x| x * 10).collect::<Vec<u64>>()
                        })
                        .logged(label_inner().as_str(), log::Level::Info)
                };

                producer(seq)
                    .mapper(move |src| {
                        scale(src)
                    })
                    .map(move |xs| {
                        if sleep_map {
                            std::thread::sleep(period * 2);
                        }
                        // xs is TinyVec<[Vec<u64>; 1]>
                        xs.iter().flatten().map(|x| x * 10).collect::<Vec<u64>>()
                    })
                    .logged(label().as_str(), log::Level::Info)
                    .accumulate()
                    .finally(move |res, _| {
                        let actual = res.into_iter().flatten().collect::<Vec<u64>>();
                        let expected = (1..=actual.len())
                            .map(|x| x as u64 * 100)
                            .collect::<Vec<_>>();
                        println!("expected {:?}", expected);
                        println!("actual   {:?}", actual);
                        println!();
                        let tol = if run_mode == RunMode::RealTime { 4 } else { 0 };
                        assert!(actual.len() + tol >= n_ticks as usize);
                        assert_eq!(expected, actual);
                    })
                    .run(run_mode, run_for)
                    .unwrap();
            };
            println!(
                "{:?}, delay={:?}, sleep_produce={}, sleep_map={}",
                run_mode, delay, sleep_produce, sleep_map
            );
            if delay == half_period && matches!(run_mode, RunMode::HistoricalFrom(_)) {
                assert!(catch_unwind(f).is_err());
            } else {
                f();
            }
        }
    }
}
