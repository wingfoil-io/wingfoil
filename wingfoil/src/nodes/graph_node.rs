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
use anyhow::anyhow;

#[derive(Debug, Default)]
enum GraphProducerStreamState<T, FUNC>
where
    T: Element + Send,
    FUNC: FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
{
    Func(FUNC),
    Handle(thread::JoinHandle<()>),
    #[default]
    Empty,
}

struct GraphProducerStream<T, FUNC>
where
    T: Element + Send,
    FUNC: FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
{
    receiver_stream: OnceCell<ReceiverStream<T>>,
    state: GraphProducerStreamState<T, FUNC>,
}

impl<T, FUNC> GraphProducerStream<T, FUNC>
where
    T: Element + Send,
    FUNC: FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
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

impl<T, FUNC> MutableNode for GraphProducerStream<T, FUNC>
where
    T: Element + Send,
    FUNC: FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
{
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![], vec![])
    }

    fn cycle(&mut self, graph_state: &mut GraphState) -> bool {
        self.receiver_stream.get_mut().unwrap().cycle(graph_state)
    }

    fn setup(&mut self, graph_state: &mut GraphState) {
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
                receiver_stream.setup(graph_state);
                self.receiver_stream.set(receiver_stream).unwrap();
                let tokio_runtime = graph_state.tokio_runtime();
                let start_time = graph_state.start_time();
                let run_mode = graph_state.run_mode();
                let task = move || {
                    let node = func().send(sender, None);
                    let mut graph = Graph::new_with(vec![node], tokio_runtime, run_mode, run_for, start_time);
                    graph.run().unwrap();
                };

                let handle = thread::spawn(task);
                self.state = GraphProducerStreamState::Handle(handle);
            }
            _ => panic!(),
        }
    }

    fn teardown(&mut self, graph_state: &mut GraphState) {
        self.receiver_stream
            .get_mut()
            .unwrap()
            .teardown(graph_state);
        let state = mem::take(&mut self.state);
        match state {
            GraphProducerStreamState::Handle(handle) => {
                handle.join().unwrap();
                self.state = GraphProducerStreamState::Empty;
            }
            _ => panic!(),
        }
    }
}

impl<T, FUNC> StreamPeekRef<TinyVec<[T; 1]>> for GraphProducerStream<T, FUNC>
where
    T: Element + Send + Hash + Eq,
    FUNC: FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
{
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        self.receiver_stream.get().unwrap().peek_ref()
    }
}


/// Creates a [Stream] emitting values on this thread
/// but produced on a worker thread.
pub fn producer<T: Element + Send + Hash + Eq>(
    func: impl FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
) -> Rc<dyn Stream<TinyVec<[T; 1]>>> {
    GraphProducerStream::new(func).into_stream()
}

#[derive(Debug, Default)]
enum GraphMapStreamState<FUNC, IN, OUT>
where
    IN: Element + Send,
    OUT: Element + Send,
    FUNC: FnOnce(Rc<dyn Stream<TinyVec<[IN; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static,
{
    Func(FUNC, ChannelSender<OUT>),
    Handle(thread::JoinHandle<()>),
    #[default]
    Empty,
    _Phantom(IN, OUT),
}

pub(crate) struct GraphMapStream<FUNC, IN, OUT>
where
    IN: Element + Send,
    OUT: Element + Send,
    FUNC: FnOnce(Rc<dyn Stream<TinyVec<[IN; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static,
{
    source: Rc<dyn Stream<IN>>,
    sender: OnceCell<ChannelSender<IN>>,
    receiver_stream: ReceiverStream<OUT>,
    state: GraphMapStreamState<FUNC, IN, OUT>,
}

impl<IN, OUT, FUNC> GraphMapStream<FUNC, IN, OUT>
where
    IN: Element + Send,
    OUT: Element + Send,
    FUNC: FnOnce(Rc<dyn Stream<TinyVec<[IN; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static,
{
    pub fn new(source: Rc<dyn Stream<IN>>, func: FUNC) -> Self {
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

impl<IN, OUT, FUNC> MutableNode for GraphMapStream<FUNC, IN, OUT>
where
    IN: Element + Send,
    OUT: Element + Send,
    FUNC: FnOnce(Rc<dyn Stream<TinyVec<[IN; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static,
{
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.source.clone()], vec![])
    }

    fn cycle(&mut self, graph_state: &mut GraphState) -> bool {
        if graph_state.ticked(self.source.clone()) {
            let res = self.sender
                .get_mut()
                .unwrap()
                .send(graph_state, self.source.peek_value());
            if res.is_err() {
            graph_state.terminate(res.map_err(|e| anyhow!(e)));
            }
        }

        self.receiver_stream.cycle(graph_state)
    }

    fn setup(&mut self, graph_state: &mut GraphState) {
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
                    let mut graph = Graph::new_with(vec![node], tokio_runtime, run_mode, run_for, start_time);
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
            _ => panic!(),
        }
        self.receiver_stream.setup(graph_state);
    }

    fn stop(&mut self, state: &mut GraphState) {
        let res = self.sender.get_mut().unwrap().close();
        if res.is_err() {
            state.terminate(res.map_err(|e| anyhow!(e)));
        }
    }

    fn teardown(&mut self, graph_state: &mut GraphState) {
        self.receiver_stream.teardown(graph_state);
        let state = mem::take(&mut self.state);
        match state {
            GraphMapStreamState::Handle(handle) => {
                handle.join().unwrap();
                self.state = GraphMapStreamState::Empty;
            }
            _ => panic!(),
        }
    }
}

impl<IN, OUT, FUNC> StreamPeekRef<TinyVec<[OUT; 1]>> for GraphMapStream<FUNC, IN, OUT>
where
    IN: Element + Send,
    OUT: Element + Send,
    FUNC: FnOnce(Rc<dyn Stream<TinyVec<[IN; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static,
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
        _ = env_logger::try_init();
        let n_runs = 1;
        let n_ticks = 5;
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
            let label = || {
                let thread_id = thread::current().id();
                let lbl = format!("{thread_id:>3?}>>");
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
            let scale = move |src: Rc<dyn Stream<TinyVec<[TinyVec<[u64; 1]>; 1]>>>| {
                src.delay(delay)
                    .map(move |xs| xs.iter().flatten().map(|x| x * 10).collect::<Vec<u64>>())
                    .logged(label().as_str(), log::Level::Info)
            };
            let f = move || {
                producer(seq)
                    .mapper(scale)
                    .map(move |xs| {
                        if sleep_map {
                            std::thread::sleep(period * 2);
                        }
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
                        assert!(actual.len() + 3 >= n_ticks as usize);
                        assert_eq!(expected, actual);
                    })
                    .run(run_mode, run_for)
                    .unwrap();
            };
            println!("{run_mode:?}, delay={delay:?}, sleep_produce={sleep_produce:}, sleep_map={sleep_map:}");
            if delay == half_period && matches!(run_mode, RunMode::HistoricalFrom(_)) {
                assert!(catch_unwind(f).is_err());
            } else {
                f();
            }
        }
    }
}
