//! Classic-API compatibility facade (Phase 6, proof of concept).
//!
//! The whole point of the port is that existing wingfoil code — and the
//! Python bindings — keep working on the new engine. Classic code is written
//! against free source functions and *runs the stream directly*:
//!
//! ```
//! use std::time::Duration;
//! use wingfoil_next::{NanoTime, RunFor, RunMode};
//! use wingfoil_next::compat::ticker;
//!
//! let counted = ticker(Duration::from_nanos(100)).count();
//! counted.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))?;
//! assert_eq!(5, counted.peek_value());
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! This module reproduces that shape over the [`Builder`](crate::interp)
//! engine. A [`Signal<T>`] wraps the fluent [`Stream`] plus the shared graph
//! and a slot for the [`Runner`] produced by `run`, so `run` / `peek_value`
//! read like the classic `Stream` API even though the engine underneath is
//! the new one. It demonstrates the facade carries the classic ergonomics;
//! the full ~40-method surface is mechanical from here.

use std::cell::RefCell;
use std::fmt::Debug;
use std::ops::{Not, Sub};
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use wingfoil_next::{NanoTime, RunFor, RunMode};

use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::interp::Runner;

/// A stream in an implicit graph, with the classic `run` / `peek_value`
/// ergonomics. Combinators mirror the classic `StreamOperators`.
pub struct Signal<T> {
    stream: Stream<T>,
    graph: GraphBuilder,
    /// The runner produced by [`Signal::run`], shared by every signal in the
    /// graph so `peek_value` works whichever one you call it on.
    runner: Rc<RefCell<Option<Runner>>>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            graph: self.graph.clone(),
            runner: self.runner.clone(),
        }
    }
}

/// A source that ticks at a fixed interval — the classic free function.
pub fn ticker(period: Duration) -> Signal<()> {
    let graph = GraphBuilder::new();
    let stream = graph.ticker(period);
    Signal {
        stream,
        graph,
        runner: Rc::new(RefCell::new(None)),
    }
}

/// A source that ticks once with `value` — the classic free function.
pub fn constant<T: Clone + Default + 'static>(value: T) -> Signal<T> {
    let graph = GraphBuilder::new();
    let stream = graph.constant(value);
    Signal {
        stream,
        graph,
        runner: Rc::new(RefCell::new(None)),
    }
}

impl<T> Signal<T> {
    fn wrap<B>(&self, stream: Stream<B>) -> Signal<B> {
        Signal {
            stream,
            graph: self.graph.clone(),
            runner: self.runner.clone(),
        }
    }
}

impl<T: 'static> Signal<T> {
    /// Apply a closure to each value.
    pub fn map<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> B + 'static,
    {
        self.wrap(self.stream.map(f))
    }

    /// Apply a fallible closure to each value; a returned `Err` aborts the
    /// run with context.
    pub fn try_map<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Result<B> + 'static,
    {
        self.wrap(self.stream.try_map(f))
    }

    /// Map and filter in one pass: `f` returns `(value, emit?)` — emit the
    /// value only when the flag is true.
    pub fn map_filter<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> (B, bool) + 'static,
    {
        self.wrap(self.stream.map_filter(f))
    }

    /// Map and filter with an `Option` (the classic `filter_map`): tick the
    /// returned `Some`, drop `None`. Delegates to the fluent
    /// [`map_filter`](StreamOps::map_filter).
    pub fn filter_map<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Option<B> + 'static,
    {
        self.wrap(self.stream.map_filter(move |v| match f(v) {
            Some(out) => (out, true),
            None => (B::default(), false),
        }))
    }

    /// Emit the result of `f()` on each tick, ignoring the value (the classic
    /// `produce`). Sugar over [`map`](StreamOps::map).
    pub fn produce<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn() -> B + 'static,
    {
        self.wrap(self.stream.map(move |_| f()))
    }

    /// Run a side-effecting fallible closure on each tick — the graph's
    /// outbound edge (the classic `for_each` / `try_for_each`). A returned
    /// `Err` aborts the run with context; emits `()` per tick.
    pub fn for_each<F>(&self, f: F) -> Signal<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        self.wrap(self.stream.for_each(f))
    }

    /// Collapse a burst/iterator value into a single tick of its **last** item
    /// (the classic `collapse`); stays quiet when the iterator is empty.
    pub fn collapse<OUT>(&self) -> Signal<OUT>
    where
        T: Clone + IntoIterator<Item = OUT>,
        OUT: Clone + Default + 'static,
    {
        self.wrap(self.stream.collapse())
    }

    /// Fold values into an accumulator, emitting it after each fold.
    pub fn fold<B, F>(&self, init: B, f: F) -> Signal<B>
    where
        B: Clone + 'static,
        F: Fn(&mut B, &T) + 'static,
    {
        self.wrap(self.stream.fold(init, f))
    }

    /// Pair each value with the current engine time: `(time, value)`.
    pub fn with_time(&self) -> Signal<(NanoTime, T)>
    where
        T: Clone,
    {
        self.wrap(self.stream.with_time())
    }

    /// Emit the current engine time whenever this signal ticks.
    pub fn ticked_at(&self) -> Signal<NanoTime> {
        self.wrap(self.stream.ticked_at())
    }

    /// Emit elapsed engine time (`now - start`) whenever this signal ticks.
    pub fn ticked_at_elapsed(&self) -> Signal<NanoTime> {
        self.wrap(self.stream.ticked_at_elapsed())
    }

    /// Run the graph to its bound, storing the runner for `peek_value`.
    ///
    /// **Re-runnable**: the graph is built once (on the first call) and the
    /// [`Runner`] is retained, so a second `run` reuses it —
    /// [`Runner::run`](crate::interp::Runner::run) restores every node's state
    /// and value slot to its wiring-time initial value first, giving each run
    /// independent, reproducible results (classic's setup-per-run semantics).
    /// This is what the wingfoil-python pytest suite — and any classic-idiom
    /// code that runs a stream more than once — depends on.
    pub fn run(&self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        let mut slot = self.runner.borrow_mut();
        if slot.is_none() {
            *slot = Some(self.graph.build());
        }
        slot.as_mut()
            .expect("runner just built")
            .run(run_mode, run_for)
    }

    /// The stream's current value after a [`run`](Signal::run).
    ///
    /// # Panics
    ///
    /// Panics if called before [`run`](Signal::run): there is no value to read
    /// until the graph has run. This mirrors the classic `Stream::peek_value`,
    /// which is infallible (returns `T`, not `Result<T>`) so the facade stays
    /// drop-in compatible; the precondition is documented and enforced with an
    /// explanatory panic rather than a bare out-of-bounds one.
    pub fn peek_value(&self) -> T
    where
        T: Clone + Default,
    {
        self.runner
            .borrow()
            .as_ref()
            .expect("Signal::run must be called before Signal::peek_value")
            .value(&self.stream)
    }
}

impl<T: Clone + Default + 'static> Signal<T> {
    /// Emit only when `condition`'s current value is true.
    pub fn filter(&self, condition: &Signal<bool>) -> Signal<T> {
        self.wrap(self.stream.filter(&condition.stream))
    }

    /// Collect every emitted value into a `Vec`.
    pub fn accumulate(&self) -> Signal<Vec<T>> {
        self.wrap(self.stream.accumulate())
    }

    /// Emit the current value whenever `trigger` ticks (passive read).
    pub fn sample(&self, trigger: &Signal<()>) -> Signal<T> {
        self.wrap(self.stream.sample(&trigger.stream))
    }

    /// Merge with another signal; the earliest-supplied ticked input wins.
    pub fn merge(&self, other: &Signal<T>) -> Signal<T> {
        self.wrap(self.stream.merge(&other.stream))
    }

    /// Pass through the first `limit` values, then stay quiet.
    pub fn limit(&self, limit: u32) -> Signal<T> {
        self.wrap(self.stream.limit(limit))
    }

    /// Rate-limit: emit at most once per `interval`.
    pub fn throttle(&self, interval: Duration) -> Signal<T> {
        self.wrap(self.stream.throttle(interval))
    }

    /// Buffer values and flush them as a `Vec` on each `interval` boundary
    /// (and once more on the last cycle).
    pub fn window(&self, interval: Duration) -> Signal<Vec<T>> {
        self.wrap(self.stream.window(interval))
    }

    /// Buffer values and flush them as a `Vec` once `capacity` accumulate
    /// (and once more on the last cycle).
    pub fn buffer(&self, capacity: usize) -> Signal<Vec<T>> {
        self.wrap(self.stream.buffer(capacity))
    }

    /// Drop values contingent on a predicate (the classic `filter_value`):
    /// keep a value only when `predicate` returns true. Delegates to the
    /// fluent [`map_filter`](StreamOps::map_filter).
    pub fn filter_value<F>(&self, predicate: F) -> Signal<T>
    where
        F: Fn(&T) -> bool + 'static,
    {
        self.wrap(self.stream.map_filter(move |v| (v.clone(), predicate(v))))
    }

    /// Fold values into an accumulator seeded from `T::default()`, applying
    /// `f(acc, value)` (the classic `reduce`). Delegates to the fluent
    /// [`fold`](StreamOps::fold).
    pub fn reduce<F>(&self, f: F) -> Signal<T>
    where
        F: Fn(&T, &T) -> T + 'static,
    {
        self.wrap(self.stream.fold(T::default(), move |acc, val| {
            *acc = f(acc, val);
        }))
    }

    /// Observe each value with a side-effecting closure, passing it through
    /// unchanged (a debug tap — the classic `inspect`).
    pub fn inspect<F>(&self, f: F) -> Signal<T>
    where
        F: Fn(&T) + 'static,
    {
        self.wrap(self.stream.inspect(f))
    }

    /// Pass each value through unchanged, printing a performance summary at
    /// the end of the run (the classic `timed`).
    pub fn timed(&self) -> Signal<T> {
        self.wrap(self.stream.timed())
    }

    /// Run `f` once at teardown — after the run ends, even if a cycle aborted
    /// it (the classic `finally`). Observes this signal's last value; emits
    /// nothing.
    pub fn finally<F>(&self, f: F) -> Signal<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        self.wrap(self.stream.finally(f))
    }
}

impl<T: Clone + Default + PartialEq + 'static> Signal<T> {
    /// Re-emit each value `delay` later.
    pub fn delay(&self, delay: Duration) -> Signal<T> {
        self.wrap(self.stream.delay(delay))
    }

    /// Suppress consecutive duplicate values (emit on change only).
    pub fn distinct(&self) -> Signal<T> {
        self.wrap(self.stream.distinct())
    }

    /// [`delay`](Signal::delay) with a reset trigger (the classic
    /// `delay_with_reset`): when `trigger` ticks, the output snaps to the
    /// current value and any pending (delayed) values are dropped. `trigger`
    /// is read for its tick only, so its value type is irrelevant.
    pub fn delay_with_reset<U: 'static>(&self, delay: Duration, trigger: &Signal<U>) -> Signal<T> {
        self.wrap(self.stream.delay_with_reset(delay, &trigger.stream))
    }
}

impl<T: Clone + Default + Debug + 'static> Signal<T> {
    /// Pass each value through unchanged while buffering it, then print the
    /// whole buffer at teardown (the classic `print`).
    pub fn print(&self) -> Signal<T> {
        self.wrap(self.stream.print())
    }
}

impl<A, B> Signal<(A, B)>
where
    A: Clone + Default + 'static,
    B: Clone + Default + 'static,
{
    /// Decompose a signal of pairs into its two component signals (the classic
    /// `split`). Both branches tick whenever the source does.
    pub fn split(&self) -> (Signal<A>, Signal<B>) {
        let (a, b) = self.stream.split();
        (self.wrap(a), self.wrap(b))
    }
}

impl<T: Clone + Default + 'static> Signal<Option<T>> {
    /// Drop `None` values, yielding a `Signal<T>` of just the `Some` payloads
    /// (the classic `filter_none`). Delegates to the fluent
    /// [`map_filter`](StreamOps::map_filter).
    pub fn filter_none(&self) -> Signal<T> {
        self.wrap(self.stream.map_filter(|opt: &Option<T>| match opt.clone() {
            Some(v) => (v, true),
            None => (T::default(), false),
        }))
    }
}

impl<T: Clone + Default + Sub<Output = T> + 'static> Signal<T> {
    /// Emit the successive difference `value - previous`; quiet on the first.
    pub fn difference(&self) -> Signal<T> {
        self.wrap(self.stream.difference())
    }
}

impl<T: Clone + Default + Not<Output = T> + 'static> Signal<T> {
    /// Negate each value (`!value`) — sugar over `map`.
    pub fn not(&self) -> Signal<T> {
        self.wrap(self.stream.not())
    }
}

impl Signal<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Signal<u64> {
        self.wrap(self.stream.count())
    }
}
