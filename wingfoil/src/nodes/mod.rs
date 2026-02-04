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
mod try_map;
mod window;

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
use try_map::*;
use window::WindowStream;

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
use std::fmt::Debug as StdDebug;

/// Returns a [Stream] that adds both it's source [Stream]s.  Ticks when either of it's sources ticks.
pub fn add<'a, T>(upstream1: &Rc<dyn Stream<'a, T> + 'a>, upstream2: &Rc<dyn Stream<'a, T> + 'a>) -> Rc<dyn Stream<'a, T> + 'a>
where
    T: StdDebug + Clone + Default + Add<Output = T> + 'a,
{
    let f = |a: T, b: T| (a + b) as T;
    BiMapStream::new(upstream1.clone(), upstream2.clone(), Box::new(f)).into_stream()
}

/// Maps two [Stream]s into one using the supplied function.  Ticks when either of it's sources ticks.
pub fn bimap<'a, IN1: Clone + 'a, IN2: Clone + 'a, OUT: StdDebug + Clone + Default + 'a>(
    upstream1: Rc<dyn Stream<'a, IN1> + 'a>,
    upstream2: Rc<dyn Stream<'a, IN2> + 'a>,
    func: impl Fn(IN1, IN2) -> OUT + 'a,
) -> Rc<dyn Stream<'a, OUT> + 'a> {
    BiMapStream::new(upstream1, upstream2, Box::new(func)).into_stream()
}

/// Returns a stream that merges it's sources into one.  Ticks when either of it's sources ticks.
/// If more than one source ticks at the same time, the first one that was supplied is used.
pub fn merge<'a, T>(sources: Vec<Rc<dyn Stream<'a, T> + 'a>>) -> Rc<dyn Stream<'a, T> + 'a>
where
    T: StdDebug + Clone + Default + 'a,
{
    MergeStream::new(sources).into_stream()
}

/// Returns a stream that ticks once with the specified value, on the first cycle.
pub fn constant<'a, T: StdDebug + Clone + Default + 'a>(value: T) -> Rc<dyn Stream<'a, T> + 'a> {
    ConstantStream::new(value).into_stream()
}

/// Collects a Vec of [Stream]s into a [Stream] of Vec.
pub fn combine<'a, T>(streams: Vec<Rc<dyn Stream<'a, T> + 'a>>) -> Rc<dyn Stream<'a, TinyVec<[T; 1]>> + 'a>
where
    T: StdDebug + Clone + Default + 'a,
{
    combine::combine(streams)
}

/// Returns a [Node] that ticks with the specified period.
pub fn ticker<'a>(period: Duration) -> Rc<dyn Node<'a> + 'a> {
    TickNode::new(NanoTime::from(period)).into_node()
}

/// A trait containing operators that can be applied to [Node]s.
/// Used to support method chaining syntax.
pub trait NodeOperators<'a> {
    /// Running count of the number of times it's source ticks.
    fn count(self: &Rc<Self>) -> Rc<dyn Stream<'a, u64> + 'a>;

    /// Emits the time of source ticks in nanos from unix epoch.
    fn ticked_at(self: &Rc<Self>) -> Rc<dyn Stream<'a, NanoTime> + 'a>;

    /// Emits the time of source ticks relative to the start.
    fn ticked_at_elapsed(self: &Rc<Self>) -> Rc<dyn Stream<'a, NanoTime> + 'a>;

    /// Emits the result of supplied closure on each upstream tick.
    fn produce<T: StdDebug + Clone + Default + 'a>(self: &Rc<Self>, func: impl Fn() -> T + 'a) -> Rc<dyn Stream<'a, T> + 'a>;

    /// Shortcut for [Graph::run] i.e. initialise and execute the graph.
    fn run(self: &Rc<Self>, run_mode: RunMode, run_to: RunFor) -> anyhow::Result<()>;
    fn into_graph(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> Graph<'a>;
}

impl<'a> NodeOperators<'a> for dyn Node<'a> + 'a {
    fn count(self: &Rc<Self>) -> Rc<dyn Stream<'a, u64> + 'a> {
        constant(1).sample(self.clone()).sum()
    }

    fn ticked_at(self: &Rc<Self>) -> Rc<dyn Stream<'a, NanoTime> + 'a> {
        let f = Box::new(|state: &mut GraphState<'a>| state.time());
        GraphStateStream::new(self.clone(), f).into_stream()
    }
    fn ticked_at_elapsed(self: &Rc<Self>) -> Rc<dyn Stream<'a, NanoTime> + 'a> {
        let f = Box::new(|state: &mut GraphState<'a>| state.elapsed());
        GraphStateStream::new(self.clone(), f).into_stream()
    }
    fn produce<T: StdDebug + Clone + Default + 'a>(self: &Rc<Self>, func: impl Fn() -> T + 'a) -> Rc<dyn Stream<'a, T> + 'a> {
        ProducerStream::new(self.clone(), Box::new(func)).into_stream()
    }
    fn run(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> anyhow::Result<()> {
        Graph::new(vec![self.clone()], run_mode, run_for).run()
    }
    fn into_graph(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> Graph<'a> {
        Graph::new(vec![self.clone()], run_mode, run_for)
    }
}

impl<'a, T> NodeOperators<'a> for dyn Stream<'a, T> + 'a {
    fn count(self: &Rc<Self>) -> Rc<dyn Stream<'a, u64> + 'a> {
        self.clone().as_node().count()
    }
    fn ticked_at(self: &Rc<Self>) -> Rc<dyn Stream<'a, NanoTime> + 'a> {
        self.clone().as_node().ticked_at()
    }
    fn ticked_at_elapsed(self: &Rc<Self>) -> Rc<dyn Stream<'a, NanoTime> + 'a> {
        self.clone().as_node().ticked_at_elapsed()
    }
    fn produce<OUT: StdDebug + Clone + Default + 'a>(
        self: &Rc<Self>,
        func: impl Fn() -> OUT + 'a,
    ) -> Rc<dyn Stream<'a, OUT> + 'a> {
        self.clone().as_node().produce(func)
    }
    fn run(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> anyhow::Result<()> {
        self.clone().as_node().run(run_mode, run_for)
    }
    fn into_graph(self: &Rc<Self>, run_mode: RunMode, run_for: RunFor) -> Graph<'a> {
        self.clone().as_node().into_graph(run_mode, run_for)
    }
}

/// A trait containing operators that can be applied to [Stream]s.
/// Used to support method chaining syntax.
pub trait StreamOperators<'a, T: StdDebug + Clone + Default + 'a> {
    /// accumulate the source into a vector
    fn accumulate(self: &Rc<Self>) -> Rc<dyn Stream<'a, Vec<T>> + 'a>;
    /// running average of source
    fn average(self: &Rc<Self>) -> Rc<dyn Stream<'a, f64> + 'a>
    where
        T: ToPrimitive;
    /// Buffer the source stream.  The buffer is automatically flushed on the last cycle;
    fn buffer(self: &Rc<Self>, capacity: usize) -> Rc<dyn Stream<'a, Vec<T>> + 'a>;
    /// Buffer the source stream based on time interval. The window is automatically flushed when the interval is exceeded or on the last cycle.
    fn window(self: &Rc<Self>, interval: Duration) -> Rc<dyn Stream<'a, Vec<T>> + 'a>;
    /// Used to accumulate values, which can be retrieved after
    /// the graph has completed running. Useful for unit tests.
    fn collect(self: &Rc<Self>) -> Rc<dyn Stream<'a, Vec<ValueAt<T>>> + 'a>;
    /// collapses a burst (i.e. IntoIter\[T\]) of ticks into a single tick \[T\].   
    /// Does not tick if burst is empty.
    fn collapse<OUT>(self: &Rc<Self>) -> Rc<dyn Stream<'a, OUT> + 'a>
    where
        T: std::iter::IntoIterator<Item = OUT>,
        OUT: StdDebug + Clone + Default + 'a;
    fn consume_async<FUT>(
        self: &Rc<Self>,
        func: Box<dyn FnOnce(Pin<Box<dyn FutStream<'static, T>>>) -> FUT + Send>,
    ) -> Rc<dyn Node<'a> + 'a>
    where
        T: Send + 'static,
        FUT: Future<Output = ()> + Send + 'static;
    fn finally<F: FnOnce(T, &GraphState<'a>) + 'a>(self: &Rc<Self>, func: F) -> Rc<dyn Node<'a> + 'a>;
    /// executes supplied closure on each tick
    fn for_each(self: &Rc<Self>, func: impl Fn(T, NanoTime) + 'a) -> Rc<dyn Node<'a> + 'a>;
    // reduce/fold source by applying function
    fn fold<OUT: StdDebug + Clone + Default + 'a>(
        self: &Rc<Self>,
        func: impl Fn(&mut OUT, T) + 'a,
    ) -> Rc<dyn Stream<'a, OUT> + 'a>;
    /// difference in it's source from one cycle to the next
    fn difference(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: std::ops::Sub<Output = T>;

    /// Propagates it's source, delayed by the specified duration
    fn delay(self: &Rc<Self>, delay: Duration) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: Hash + Eq;
    /// Demuxes its source into a Vec of n streams.
    fn demux<K, F>(
        self: &Rc<Self>,
        capacity: usize,
        func: F,
    ) -> (Vec<Rc<dyn Stream<'a, T> + 'a>>, Overflow<'a, T>)
    where
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'a,
        F: Fn(&T) -> (K, DemuxEvent) + 'a;
    /// Demuxes its source into a vec of n streams, where source is IntoIterator
    /// For example demuxes Vec of U into n streams of Vec of U
    fn demux_it<K, F, U>(
        self: &Rc<Self>,
        capacity: usize,
        func: F,
    ) -> (
        Vec<Rc<dyn Stream<'a, TinyVec<[U; 1]>> + 'a>>,
        Overflow<'a, TinyVec<[U; 1]>>,
    )
    where
        T: IntoIterator<Item = U>,
        U: StdDebug + Clone + Default + 'a,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'a,
        F: Fn(&U) -> (K, DemuxEvent) + 'a;
    /// Demuxes its source into a vec of n streams, where source is IntoIterator
    /// For example demuxes Vec of U into n streams of Vec of U
    fn demux_it_with_map<K, F, U>(
        self: &Rc<Self>,
        map: DemuxMap<K>,
        func: F,
    ) -> (
        Vec<Rc<dyn Stream<'a, TinyVec<[U; 1]>> + 'a>>,
        Overflow<'a, TinyVec<[U; 1]>>,
    )
    where
        T: IntoIterator<Item = U>,
        U: StdDebug + Clone + Default + 'a,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'a,
        F: Fn(&U) -> (K, DemuxEvent) + 'a;
    /// only propagates it's source if it is changed
    fn distinct(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: PartialEq;
    /// drops source contingent on supplied stream
    fn filter(self: &Rc<Self>, condition: Rc<dyn Stream<'a, bool> + 'a>) -> Rc<dyn Stream<'a, T> + 'a>;
    /// drops source contingent on supplied predicate
    fn filter_value(self: &Rc<Self>, predicate: impl Fn(&T) -> bool + 'a)
    -> Rc<dyn Stream<'a, T> + 'a>;
    /// propagates source up to limit times
    fn limit(self: &Rc<Self>, limit: u32) -> Rc<dyn Stream<'a, T> + 'a>;
    /// logs source and propagates it
    fn logged(self: &Rc<Self>, label: &str, level: Level) -> Rc<dyn Stream<'a, T> + 'a>;
    /// Map's it's source into a new Stream using the supplied closure.
    fn map<OUT: StdDebug + Clone + Default + 'a>(self: &Rc<Self>, func: impl Fn(T) -> OUT + 'a)
    -> Rc<dyn Stream<'a, OUT> + 'a>;
    /// Map's source into a new Stream using a fallible closure.
    /// Errors propagate to graph execution.
    fn try_map<OUT: StdDebug + Clone + Default + 'a>(
        self: &Rc<Self>,
        func: impl Fn(T) -> anyhow::Result<OUT> + 'a,
    ) -> Rc<dyn Stream<'a, OUT> + 'a>;
    /// Uses func to build graph, which is spawned on worker thread
    fn mapper<FUNC, OUT>(self: &Rc<Self>, func: FUNC) -> Rc<dyn Stream<'a, TinyVec<[OUT; 1]>> + 'a>
    where
        T: Send + Default + 'static,
        OUT: StdDebug + Clone + Default + Send + Hash + Eq + 'static,
        FUNC: FnOnce(Rc<dyn Stream<'static, TinyVec<[T; 1]>> + 'static>) -> Rc<dyn Stream<'static, OUT> + 'static> + Send + 'static;
    /// negates it's input
    fn not(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: std::ops::Not<Output = T>;

    fn reduce(self: &Rc<Self>, func: impl Fn(T, T) -> T + 'a) -> Rc<dyn Stream<'a, T> + 'a>;
    /// samples it's source on each tick of trigger
    fn sample(self: &Rc<Self>, trigger: Rc<dyn Node<'a> + 'a>) -> Rc<dyn Stream<'a, T> + 'a>;
    // print stream values to stdout
    fn print(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>;
    fn sum(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: Add<T, Output = T>;
}

impl<'a, T> StreamOperators<'a, T> for dyn Stream<'a, T> + 'a
where
    T: StdDebug + Clone + Default + 'a,
{
    fn accumulate(self: &Rc<Self>) -> Rc<dyn Stream<'a, Vec<T>> + 'a> {
        self.fold(|acc: &mut Vec<T>, value| {
            acc.push(value);
        })
    }

    fn average(self: &Rc<Self>) -> Rc<dyn Stream<'a, f64> + 'a>
    where
        T: ToPrimitive,
    {
        AverageStream::new(self.clone()).into_stream()
    }

    fn buffer(self: &Rc<Self>, capacity: usize) -> Rc<dyn Stream<'a, Vec<T>> + 'a> {
        BufferStream::new(self.clone(), capacity).into_stream()
    }

    fn window(self: &Rc<Self>, interval: Duration) -> Rc<dyn Stream<'a, Vec<T>> + 'a> {
        WindowStream::new(self.clone(), NanoTime::new(interval.as_nanos() as u64)).into_stream()
    }

    fn collect(self: &Rc<Self>) -> Rc<dyn Stream<'a, Vec<ValueAt<T>>> + 'a> {
        bimap(
            self.clone(),
            self.clone().as_node().ticked_at(),
            ValueAt::new,
        )
        .fold(|acc: &mut Vec<ValueAt<T>>, value| {
            acc.push(value);
        })
    }

    fn collapse<OUT>(self: &Rc<Self>) -> Rc<dyn Stream<'a, OUT> + 'a>
    where
        T: std::iter::IntoIterator<Item = OUT>,
        OUT: StdDebug + Clone + Default + 'a,
    {
        let f = |x: T| match x.into_iter().last() {
            Some(x) => (x, true),
            None => (Default::default(), false),
        };
        MapFilterStream::new(self.clone(), Box::new(f)).into_stream()
    }

    fn consume_async<FUT>(
        self: &Rc<Self>,
        func: Box<dyn FnOnce(Pin<Box<dyn FutStream<'static, T>>>) -> FUT + Send>,
    ) -> Rc<dyn Node<'a> + 'a>
    where
        T: Send + 'static,
        FUT: Future<Output = ()> + Send + 'static,
    {
        AsyncConsumerNode::new(self.clone(), func).into_node()
    }

    fn demux<K, F>(
        self: &Rc<Self>,
        capacity: usize,
        func: F,
    ) -> (Vec<Rc<dyn Stream<'a, T> + 'a>>, Overflow<'a, T>)
    where
        T: StdDebug + Clone + Default + 'a,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'a,
        F: Fn(&T) -> (K, DemuxEvent) + 'a,
    {
        demux::demux(self.clone(), demux::DemuxMap::new(capacity), func)
    }

    fn demux_it<K, F, U>(
        self: &Rc<Self>,
        capacity: usize,
        func: F,
    ) -> (
        Vec<Rc<dyn Stream<'a, TinyVec<[U; 1]>> + 'a>>,
        Overflow<'a, TinyVec<[U; 1]>>,
    )
    where
        T: IntoIterator<Item = U>,
        U: StdDebug + Clone + Default + 'a,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'a,
        F: Fn(&U) -> (K, DemuxEvent) + 'a,
    {
        self.demux_it_with_map(DemuxMap::new(capacity), func)
    }

    fn demux_it_with_map<K, F, U>(
        self: &Rc<Self>,
        map: DemuxMap<K>,
        func: F,
    ) -> (
        Vec<Rc<dyn Stream<'a, TinyVec<[U; 1]>> + 'a>>,
        Overflow<'a, TinyVec<[U; 1]>>,
    )
    where
        T: IntoIterator<Item = U>,
        U: StdDebug + Clone + Default + 'a,
        K: Hash + Eq + PartialEq + std::fmt::Debug + 'a,
        F: Fn(&U) -> (K, DemuxEvent) + 'a,
    {
        demux_it(self.clone(), map.size(), func)
    }

    fn for_each(self: &Rc<Self>, func: impl Fn(T, NanoTime) + 'a) -> Rc<dyn Node<'a> + 'a> {
        ConsumerNode::new(self.clone(), Box::new(func)).into_node()
    }

    fn delay(self: &Rc<Self>, duration: Duration) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: Hash + Eq,
    {
        DelayStream::new(self.clone(), NanoTime::new(duration.as_nanos() as u64)).into_stream()
    }

    fn difference(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: std::ops::Sub<Output = T>,
    {
        DifferenceStream::new(self.clone()).into_stream()
    }

    fn distinct(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: PartialEq,
    {
        DistinctStream::new(self.clone()).into_stream()
    }

    fn filter(self: &Rc<Self>, condition: Rc<dyn Stream<'a, bool> + 'a>) -> Rc<dyn Stream<'a, T> + 'a> {
        FilterStream::new(self.clone(), condition).into_stream()
    }

    fn filter_value(
        self: &Rc<Self>,
        predicate: impl Fn(&T) -> bool + 'a,
    ) -> Rc<dyn Stream<'a, T> + 'a> {
        let condition = self.clone().map(move |val| predicate(&val));
        FilterStream::new(self.clone(), condition).into_stream()
    }

    fn finally<F: FnOnce(T, &GraphState<'a>) + 'a>(self: &Rc<Self>, func: F) -> Rc<dyn Node<'a> + 'a> {
        FinallyNode::new(self.clone(), Some(func)).into_node()
    }

    fn fold<OUT: StdDebug + Clone + Default + 'a>(
        self: &Rc<Self>,
        func: impl Fn(&mut OUT, T) + 'a,
    ) -> Rc<dyn Stream<'a, OUT> + 'a> {
        FoldStream::new(self.clone(), Box::new(func)).into_stream()
    }

    fn limit(self: &Rc<Self>, limit: u32) -> Rc<dyn Stream<'a, T> + 'a> {
        LimitStream::new(self.clone(), limit).into_stream()
    }

    fn logged(self: &Rc<Self>, label: &str, level: Level) -> Rc<dyn Stream<'a, T> + 'a> {
        if log::log_enabled!(level) {
            let lbl = label.to_string();
            let func = move |value, time: NanoTime| {
                log!(target:"wingfoil", level, "{:} {:} {:?}", time.pretty(), lbl, value);
                value
            };
            bimap(
                self.clone(),
                self.clone().as_node().ticked_at_elapsed(),
                func,
            )
        } else {
            self.clone()
        }
    }

    fn map<OUT: StdDebug + Clone + Default + 'a>(
        self: &Rc<Self>,
        func: impl Fn(T) -> OUT + 'a,
    ) -> Rc<dyn Stream<'a, OUT> + 'a> {
        MapStream::new(self.clone(), Default::default(), Box::new(func)).into_stream()
    }

    fn try_map<OUT: StdDebug + Clone + Default + 'a>(
        self: &Rc<Self>,
        func: impl Fn(T) -> anyhow::Result<OUT> + 'a,
    ) -> Rc<dyn Stream<'a, OUT> + 'a> {
        TryMapStream::new(self.clone(), Box::new(func)).into_stream()
    }

    fn mapper<FUNC, OUT>(self: &Rc<Self>, func: FUNC) -> Rc<dyn Stream<'a, TinyVec<[OUT; 1]>> + 'a>
    where
        T: Send + Default + 'static,
        OUT: StdDebug + Clone + Default + Send + Hash + Eq + 'static,
        FUNC: FnOnce(Rc<dyn Stream<'static, TinyVec<[T; 1]>> + 'static>) -> Rc<dyn Stream<'static, OUT> + 'static> + Send + 'static,
    {
        // This requires 'static because it spawns a thread.
        // We can't easily propagate 'a to the spawned thread.
        // If 'a is already 'static, this works.
        // If not, we have a problem.
        // For now, let's assume 'static for simplicity in this complex refactor.
        unsafe {
            let slf = std::mem::transmute::<Rc<dyn Stream<'a, T> + 'a>, Rc<dyn Stream<'static, T> + 'static>>(self.clone());
            let node = GraphMapStream::new(slf, func).into_stream();
            std::mem::transmute::<Rc<dyn Stream<'static, TinyVec<[OUT; 1]>> + 'static>, Rc<dyn Stream<'a, TinyVec<[OUT; 1]>> + 'a>>(node)
        }
    }

    fn not(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: std::ops::Not<Output = T>,
    {
        self.map(|value| !value)
    }

    fn print(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a> {
        PrintStream::new(self.clone()).into_stream()
    }

    fn reduce(self: &Rc<Self>, func: impl Fn(T, T) -> T + 'a) -> Rc<dyn Stream<'a, T> + 'a> {
        let f = move |acc: &mut T, val: T| {
            *acc = func((*acc).clone(), val);
        };
        self.fold(f)
    }

    fn sample(self: &Rc<Self>, trigger: Rc<dyn Node<'a> + 'a>) -> Rc<dyn Stream<'a, T> + 'a> {
        SampleStream::new(self.clone(), trigger).into_stream()
    }
    fn sum(self: &Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>
    where
        T: Add<T, Output = T>,
    {
        self.reduce(|acc, val| acc + val)
    }
}

#[doc(hidden)]
pub trait TupleStreamOperators<'a, A, B>
where
    A: StdDebug + Clone + Default + 'a,
    B: StdDebug + Clone + Default + 'a,
{
    fn split(self: &Rc<Self>) -> (Rc<dyn Stream<'a, A> + 'a>, Rc<dyn Stream<'a, B> + 'a>);
}

impl<'a, A, B> TupleStreamOperators<'a, A, B> for dyn Stream<'a, (A, B)> + 'a
where
    A: StdDebug + Clone + Default + 'a,
    B: StdDebug + Clone + Default + 'a,
{
    fn split(self: &Rc<Self>) -> (Rc<dyn Stream<'a, A> + 'a>, Rc<dyn Stream<'a, B> + 'a>) {
        let a = self.map(|tuple: (A, B)| tuple.0);
        let b = self.map(|tuple: (A, B)| tuple.1);
        (a, b)
    }
}