//! A library of Stream and Node operators and functions.
//!

mod always;
mod async_io;
mod average;
mod bimap;
mod buffer;
mod callback;
mod channel;
mod combine;
mod constant;
mod consumer;
mod delay;
mod demux;
mod difference;
mod distinct;
mod filter;
mod finally;
mod fold;
mod graph_node;
mod graph_state;
mod limit;
mod map;
mod map_filter;
mod merge;
mod print;
mod producer;
mod sample;
mod tick;

pub use always::*;
pub use async_io::*;
pub use callback::CallBackStream;
pub use demux::*;
pub use graph_node::*;

use average::*;
use bimap::*;
use buffer::BufferStream;
use constant::*;
use consumer::*;
use delay::*;
use difference::*;
use distinct::*;
use filter::*;
use finally::*;
use fold::*;
use graph_state::*;
use limit::*;
use map::*;
use map_filter::*;
use merge::*;
use print::*;
use producer::*;
use sample::*;
use tick::*;

use crate::graph::*;
use crate::queue::ValueAt;
use crate::types::*;

use log::Level;
use log::log;
use num_traits::ToPrimitive;
use std::cmp::Eq;
use std::future::Future;
use std::hash::Hash;
use std::ops::Add;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;
use tinyvec::TinyVec;

/// Returns a [Stream] that adds both it's source [Stream]s.  Ticks when either of it's sources ticks.
pub fn add<T>(upstream1: &Rc<dyn Stream<T>>, upstream2: &Rc<dyn Stream<T>>) -> Rc<dyn Stream<T>>
where
    T: Element + Add<Output = T>,
{
    let f = |a: T, b: T| (a + b) as T;
    BiMapStream::new(upstream1.clone(), upstream2.clone(), Box::new(f)).into_stream()
}

/// Maps two [Stream]s into one using thr supplied function.  Ticks when either of it's sources ticks.
pub fn bimap<IN1: Element, IN2: Element, OUT: Element>(
    upstream1: Rc<dyn Stream<IN1>>,
    upstream2: Rc<dyn Stream<IN2>>,
    func: impl Fn(IN1, IN2) -> OUT + 'static,
) -> Rc<dyn Stream<OUT>> {
    BiMapStream::new(upstream1, upstream2, Box::new(func)).into_stream()
}

/// Returns a stream that merges it's sources into one.  Ticks when either of it's sources ticks.
/// If more than one source ticks at the same time, the first one that was supplied is used.
pub fn merge<T>(sources: Vec<Rc<dyn Stream<T>>>) -> Rc<dyn Stream<T>>
where
    T: Element,
{
    MergeStream::new(sources).into_stream()
}

/// Returns a stream that ticks once with the specified value, on the first cycle.
pub fn constant<T: Element>(value: T) -> Rc<dyn Stream<T>> {
    ConstantStream::new(value).into_stream()
}

/// Collects a Vec of [Stream]s into a [Stream] of Vec.
pub fn combine<T>(streams: Vec<Rc<dyn Stream<T>>>) -> Rc<dyn Stream<TinyVec<[T; 1]>>>
where
    T: Element + 'static,
{
    combine::combine(streams)
}

/// Returns a [Node] that ticks with the specified period.
pub fn ticker(period: Duration) -> Rc<dyn Node> {
    TickNode::new(NanoTime::new(period.as_nanos() as u64)).into_node()
}

/// A trait containing operators that can be applied to [Node]s.
/// Used to support method chaining syntax.
pub trait NodeOperators {
    /// Running count of the number of times it's source ticks.
    /// ```
    /// # use wingfoil::*;
    /// # use std::time::Duration;
    /// // 1, 2, 3, etc.
    /// ticker(Duration::from_millis(10)).count();
    /// ```
    fn count(self: &Rc<Self>) -> Rc<dyn Stream<u64>>;

    /// Emits the time of source ticks in nanos from unix epoch.
    /// ```
    /// # use wingfoil::*;
    /// # use std::time::Duration;
    /// // 0, 1000000000, 2000000000, etc.
    /// ticker(Duration::from_millis(10)).ticked_at();
    /// ```
    fn ticked_at(self: &Rc<Self>) -> Rc<dyn Stream<NanoTime>>;

    /// Emits the time of source ticks relative to the start.
    /// ```
    /// # use wingfoil::*;
    /// # use std::time::Duration;
    /// // 0, 1000000000, 2000000000, etc.
    /// ticker(Duration::from_millis(10)).ticked_at_elapsed();
    /// ```
    fn ticked_at_elapsed(self: &Rc<Self>) -> Rc<dyn Stream<NanoTime>>;

    /// Emits the result of supplied closure on each upstream tick.
    /// ```
    /// # use wingfoil::*;
    /// # use std::time::Duration;
    /// /// "hello world", "hello world", etc.
    /// ticker(Duration::from_millis(10)).produce(|| "hello, world");
    /// ```
    fn produce<T: Element>(self: &Rc<Self>, func: impl Fn() -> T + 'static) -> Rc<dyn Stream<T>>;

    /// Shortcut for [Graph::run] i.e. initialise and execute the graph.
    /// ```
    /// # use wingfoil::*;
    /// # use std::time::Duration;
    /// let count = ticker(Duration::from_millis(1))
    ///     .count();
    /// count.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
    ///     .unwrap();
    /// count.peek_value(); // 3
    /// ```
    fn run(self: &Rc<Self>, run_mode: RunMode, run_to: RunFor) -> anyhow::Result<()>;
    fn into_graph(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> Graph;
}

impl NodeOperators for dyn Node {
    fn count(self: &Rc<Self>) -> Rc<dyn Stream<u64>> {
        constant(1).sample(self.clone()).sum()
    }

    fn ticked_at(self: &Rc<Self>) -> Rc<dyn Stream<NanoTime>> {
        let f = Box::new(|state: &mut GraphState| state.time());
        GraphStateStream::new(self.clone(), f).into_stream()
    }
    fn ticked_at_elapsed(self: &Rc<Self>) -> Rc<dyn Stream<NanoTime>> {
        let f = Box::new(|state: &mut GraphState| state.elapsed());
        GraphStateStream::new(self.clone(), f).into_stream()
    }
    fn produce<T: Element>(self: &Rc<Self>, func: impl Fn() -> T + 'static) -> Rc<dyn Stream<T>> {
        ProducerStream::new(self.clone(), Box::new(func)).into_stream()
    }
    fn run(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> anyhow::Result<()> {
        Graph::new(vec![self.clone()], run_mode, run_for).run()
    }
    fn into_graph(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> Graph {
        Graph::new(vec![self.clone()], run_mode, run_for)
    }
}

impl<T> NodeOperators for dyn Stream<T> {
    fn count(self: &Rc<Self>) -> Rc<dyn Stream<u64>> {
        self.clone().as_node().count()
    }
    fn ticked_at(self: &Rc<Self>) -> Rc<dyn Stream<NanoTime>> {
        self.clone().as_node().ticked_at()
    }
    fn ticked_at_elapsed(self: &Rc<Self>) -> Rc<dyn Stream<NanoTime>> {
        self.clone().as_node().ticked_at_elapsed()
    }
    fn produce<OUT: Element>(self: &Rc<Self>, func: impl Fn() -> OUT + 'static) -> Rc<dyn Stream<OUT>> {
        self.clone().as_node().produce(func)
    }
    fn run(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> anyhow::Result<()> {
        self.clone().as_node().run(run_mode, run_for)
    }
    fn into_graph(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> Graph {
        self.clone().as_node().into_graph(run_mode, run_for)
    }
}

/// A trait containing operators that can be applied to [Stream]s.
/// Used to support method chaining syntax.
pub trait StreamOperators<T: Element> {
    /// accumulate the source into a vector
    fn accumulate(self: &Rc<Self>) -> Rc<dyn Stream<Vec<T>>>;
    /// running average of source
    fn average(self: &Rc<Self>) -> Rc<dyn Stream<f64>>
    where
        T: ToPrimitive;
    /// Buffer the source stream.  The buffer is automatically flushed on the last cycle;
    fn buffer(self: &Rc<Self>, capacity: usize) -> Rc<dyn Stream<Vec<T>>>;
    /// Used to accumulate values, which can be retrieved after
    /// the graph has completed running. Useful for unit tests.
    fn collect(self: &Rc<Self>) -> Rc<dyn Stream<Vec<ValueAt<T>>>>;
    /// collapses a burst (i.e. IntoIter\[T\]) of ticks into a single tick \[T\].   
    /// Does not tick if burst is empty.
    fn collapse<OUT>(self: &Rc<Self>) -> Rc<dyn Stream<OUT>>
    where
        T: std::iter::IntoIterator<Item = OUT>,
        OUT: Element;
    fn consume_async<FUT>(
        self: &Rc<Self>,
        func: Box<dyn FnOnce(Pin<Box<dyn FutStream<T>>>) -> FUT + Send>,
    ) -> Rc<dyn Node>
    where
        T: Element + Send,
        FUT: Future<Output = ()> + Send + 'static;
    fn finally<F: FnOnce(T, &GraphState) + Clone + 'static>(self: &Rc<Self>, func: F) -> Rc<dyn Node>;
    /// executes supplied closure on each tick
    fn for_each(self: &Rc<Self>, func: impl Fn(T, NanoTime) + 'static) -> Rc<dyn Node>;
    // reduce/fold source by applying function
    fn fold<OUT: Element>(self: &Rc<Self>, func: impl Fn(&mut OUT, T) + 'static) -> Rc<dyn Stream<OUT>>;
    /// difference in it's source from one cycle to the next
    fn difference(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: std::ops::Sub<Output = T>;

    /// Propagates it's source, delayed by the specified duration
    fn delay(self: &Rc<Self>, delay: Duration) -> Rc<dyn Stream<T>>
    where
        T: Hash + Eq;
    /// Demuxes its source into a Vec of n streams.
    fn demux<K, F>(self: &Rc<Self>, capacity: usize, func: F) -> (Vec<Rc<dyn Stream<T>>>, Overflow<T>)
    where
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'static,
        F: Fn(&T) -> (K, DemuxEvent) + 'static;
    /// Demuxes its source into a vec of n streams, where source is IntoIterator
    /// For example demuxes Vec of U into n streams of Vec of U
    fn demux_it<K, F, U>(
        self: &Rc<Self>,
        capacity: usize,
        func: F,
    ) -> (Vec<Rc<dyn Stream<TinyVec<[U; 1]>>>>, Overflow<TinyVec<[U; 1]>>)
    where
        T: IntoIterator<Item = U>,
        U: Element,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'static,
        F: Fn(&U) -> (K, DemuxEvent) + 'static;
    /// Demuxes its source into a vec of n streams, where source is IntoIterator
    /// For example demuxes Vec of U into n streams of Vec of U
    fn demux_it_with_map<K, F, U>(
        self: &Rc<Self>,
        map: DemuxMap<K>,
        func: F,
    ) -> (Vec<Rc<dyn Stream<TinyVec<[U; 1]>>>>, Overflow<TinyVec<[U; 1]>>)
    where
        T: IntoIterator<Item = U>,
        U: Element,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'static,
        F: Fn(&U) -> (K, DemuxEvent) + 'static;
    /// only propagates it's source if it is changed
    fn distinct(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: PartialEq;
    /// drops source contingent on supplied stream
    fn filter(self: &Rc<Self>, condition: Rc<dyn Stream<bool>>) -> Rc<dyn Stream<T>>;
    /// drops source contingent on supplied predicate
    fn filter_value(self: &Rc<Self>, predicate: impl Fn(&T) -> bool + 'static) -> Rc<dyn Stream<T>>;
    /// propagates source up to limit times
    fn limit(self: &Rc<Self>, limit: u32) -> Rc<dyn Stream<T>>;
    /// logs source and propagates it
    fn logged(self: &Rc<Self>, label: &str, level: Level) -> Rc<dyn Stream<T>>;
    /// Map’s it’s source into a new Stream using the supplied closure.
    fn map<OUT: Element>(self: &Rc<Self>, func: impl Fn(T) -> OUT + 'static) -> Rc<dyn Stream<OUT>>;
    /// Uses func to build graph, which is spawned on worker thread
    fn mapper<FUNC, OUT>(self: &Rc<Self>, func: FUNC) -> Rc<dyn Stream<TinyVec<[OUT; 1]>>>
    where
        T: Element + Send,
        OUT: Element + Send + Hash + Eq,
        FUNC: FnOnce(Rc<dyn Stream<TinyVec<[T; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static;
    /// negates it's input
    fn not(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: std::ops::Not<Output = T>;

    fn reduce(self: &Rc<Self>, func: impl Fn(T, T) -> T + 'static) -> Rc<dyn Stream<T>>;
    /// samples it's source on each tick of trigger
    fn sample(self: &Rc<Self>, trigger: Rc<dyn Node>) -> Rc<dyn Stream<T>>;
    // print stream values to stdout
    fn print(self: &Rc<Self>) -> Rc<dyn Stream<T>>;
    fn sum(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: Add<T, Output = T>;
}

impl<T> StreamOperators<T> for dyn Stream<T>
where
    T: Element + 'static,
{
    fn accumulate(self: &Rc<Self>) -> Rc<dyn Stream<Vec<T>>> {
        self.fold(|acc: &mut Vec<T>, value| {
            acc.push(value);
        })
    }

    fn average(self: &Rc<Self>) -> Rc<dyn Stream<f64>>
    where
        T: ToPrimitive,
    {
        AverageStream::new(self.clone()).into_stream()
    }

    fn buffer(self: &Rc<Self>, capacity: usize) -> Rc<dyn Stream<Vec<T>>> {
        BufferStream::new(self.clone(), capacity).into_stream()
    }

    fn collect(self: &Rc<Self>) -> Rc<dyn Stream<Vec<ValueAt<T>>>> {
        bimap(self.clone(), self.clone().as_node().ticked_at(), ValueAt::new).fold(
            |acc: &mut Vec<ValueAt<T>>, value| {
                acc.push(value);
            },
        )
    }

    fn collapse<OUT>(self: &Rc<Self>) -> Rc<dyn Stream<OUT>>
    where
        T: std::iter::IntoIterator<Item = OUT>,
        OUT: Element,
    {
        let f = |x: T| match x.into_iter().last() {
            Some(x) => (x, true),
            None => (Default::default(), false),
        };
        MapFilterStream::new(self.clone(), Box::new(f)).into_stream()
    }

    fn consume_async<FUT>(
        self: &Rc<Self>,
        func: Box<dyn FnOnce(Pin<Box<dyn FutStream<T>>>) -> FUT + Send>,
    ) -> Rc<dyn Node>
    where
        T: Element + Send,
        FUT: Future<Output = ()> + Send + 'static,
    {
        AsyncConsumerNode::new(self.clone(), func).into_node()
    }

    fn demux<K, F>(self: &Rc<Self>, capacity: usize, func: F) -> (Vec<Rc<dyn Stream<T>>>, Overflow<T>)
    where
        T: Element,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'static,
        F: Fn(&T) -> (K, DemuxEvent) + 'static,
    {
        demux::demux(self.clone(), demux::DemuxMap::new(capacity), func)
    }

    fn demux_it<K, F, U>(
        self: &Rc<Self>,
        capacity: usize,
        func: F,
    ) -> (Vec<Rc<dyn Stream<TinyVec<[U; 1]>>>>, Overflow<TinyVec<[U; 1]>>)
    where
        T: IntoIterator<Item = U>,
        U: Element,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'static,
        F: Fn(&U) -> (K, DemuxEvent) + 'static,
    {
        self.demux_it_with_map(DemuxMap::new(capacity), func)
    }

    fn demux_it_with_map<K, F, U>(
        self: &Rc<Self>,
        map: DemuxMap<K>,
        func: F,
    ) -> (Vec<Rc<dyn Stream<TinyVec<[U; 1]>>>>, Overflow<TinyVec<[U; 1]>>)
    where
        T: IntoIterator<Item = U>,
        U: Element,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'static,
        F: Fn(&U) -> (K, DemuxEvent) + 'static,
    {
        demux_it(self.clone(), map, func)
    }

    fn for_each(self: &Rc<Self>, func: impl Fn(T, NanoTime) + 'static) -> Rc<dyn Node> {
        ConsumerNode::new(self.clone(), Box::new(func)).into_node()
    }

    fn delay(self: &Rc<Self>, duration: Duration) -> Rc<dyn Stream<T>>
    where
        T: Hash + Eq,
    {
        DelayStream::new(self.clone(), NanoTime::new(duration.as_nanos() as u64)).into_stream()
    }

    fn difference(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: std::ops::Sub<Output = T>,
    {
        DifferenceStream::new(self.clone()).into_stream()
    }

    fn distinct(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: PartialEq,
    {
        DistinctStream::new(self.clone()).into_stream()
    }

    fn filter(self: &Rc<Self>, condition: Rc<dyn Stream<bool>>) -> Rc<dyn Stream<T>> {
        FilterStream::new(self.clone(), condition).into_stream()
    }

    fn filter_value(self: &Rc<Self>, predicate: impl Fn(&T) -> bool + 'static) -> Rc<dyn Stream<T>> {
        let condition = self.clone().map(move |val| predicate(&val));
        FilterStream::new(self.clone(), condition).into_stream()
    }

    fn finally<F: FnOnce(T, &GraphState) + Clone + 'static>(self: &Rc<Self>, func: F) -> Rc<dyn Node> {
        FinallyNode::new(self.clone(), func).into_node()
    }

    fn fold<OUT: Element>(self: &Rc<Self>, func: impl Fn(&mut OUT, T) + 'static) -> Rc<dyn Stream<OUT>> {
        FoldStream::new(self.clone(), Box::new(func)).into_stream()
    }

    fn limit(self: &Rc<Self>, limit: u32) -> Rc<dyn Stream<T>> {
        LimitStream::new(self.clone(), limit).into_stream()
    }

    fn logged(self: &Rc<Self>, label: &str, level: Level) -> Rc<dyn Stream<T>> {
        if log::log_enabled!(level) {
            let lbl = label.to_string();
            let func = move |value, time: NanoTime| {
                log!(target:"wingfoil", level, "{:} {:} {:?}", time.pretty(), lbl, value);
                value
            };
            bimap(self.clone(), self.clone().as_node().ticked_at_elapsed(), func)
        } else {
            self.clone()
        }
    }

    fn map<OUT: Element>(self: &Rc<Self>, func: impl Fn(T) -> OUT + 'static) -> Rc<dyn Stream<OUT>> {
        MapStream::new(self.clone(), Box::new(func)).into_stream()
    }

    fn mapper<FUNC, OUT>(self: &Rc<Self>, func: FUNC) -> Rc<dyn Stream<TinyVec<[OUT; 1]>>>
    where
        T: Element + Send,
        OUT: Element + Send + Hash + Eq,
        FUNC: FnOnce(Rc<dyn Stream<TinyVec<[T; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static,
    {
        GraphMapStream::new(self.clone(), func).into_stream()
    }

    fn not(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: std::ops::Not<Output = T>,
    {
        self.map(|value| !value)
    }

    fn print(self: &Rc<Self>) -> Rc<dyn Stream<T>> {
        PrintStream::new(self.clone()).into_stream()
    }

    fn reduce(self: &Rc<Self>, func: impl Fn(T, T) -> T + 'static) -> Rc<dyn Stream<T>> {
        let f = move |acc: &mut T, val: T| {
            *acc = func((*acc).clone(), val);
        };
        self.fold(f)
    }

    fn sample(self: &Rc<Self>, trigger: Rc<dyn Node>) -> Rc<dyn Stream<T>> {
        SampleStream::new(self.clone(), trigger).into_stream()
    }
    fn sum(self: &Rc<Self>) -> Rc<dyn Stream<T>>
    where
        T: Add<T, Output = T>,
    {
        self.reduce(|acc, val| acc + val)
    }
}


#[doc(hidden)]
pub trait TupleStreamOperators<A, B>
where
    A: Element + 'static,
    B: Element + 'static,
{
    fn split(self: &Rc<Self>) -> (Rc<dyn Stream<A>>, Rc<dyn Stream<B>>);
}

impl<A, B> TupleStreamOperators<A, B> for dyn Stream<(A, B)>
where
    A: Element + 'static,
    B: Element + 'static,
{
    fn split(self: &Rc<Self>) -> (Rc<dyn Stream<A>>, Rc<dyn Stream<B>>) {
        let a = self.map(|tuple: (A, B)| tuple.0);
        let b = self.map(|tuple: (A, B)| tuple.1);
        (a, b)
    }
}
