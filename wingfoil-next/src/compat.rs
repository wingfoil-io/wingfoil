//! Classic-API compatibility facade (Phase 6, proof of concept).
//!
//! The whole point of the port is that existing wingfoil code — and the
//! Python bindings — keep working on the new engine. Classic code is written
//! against free source functions and *runs the stream directly*:
//!
//! ```
//! use std::time::Duration;
//! use wingfoil::{NanoTime, RunFor, RunMode};
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
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use wingfoil::{RunFor, RunMode};

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

    /// Fold values into an accumulator, emitting it after each fold.
    pub fn fold<B, F>(&self, init: B, f: F) -> Signal<B>
    where
        B: Clone + 'static,
        F: Fn(&mut B, &T) + 'static,
    {
        self.wrap(self.stream.fold(init, f))
    }

    /// Run the graph to its bound, storing the runner for `peek_value`.
    ///
    /// Re-running is not supported: the shared [`GraphBuilder`] is consumed by
    /// the first `run`, so a second call would build — and silently run — an
    /// empty graph, then read a now-dangling handle out of bounds. Until re-run
    /// support lands (deferred — see `docs/fable-review.md`, plan point 3) a
    /// second call is a *reachable* user error, not an unreachable invariant,
    /// so it is surfaced as an error rather than a panic. The first run's
    /// runner is left in place, so `peek_value` keeps working.
    pub fn run(&self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        if self.runner.borrow().is_some() {
            anyhow::bail!(
                "Signal::run called more than once: re-running a graph is not \
                 supported (the builder is consumed by the first run)"
            );
        }
        let mut runner = self.graph.build();
        let result = runner.run(run_mode, run_for);
        *self.runner.borrow_mut() = Some(runner);
        result
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
}

impl<T: Clone + Default + PartialEq + 'static> Signal<T> {
    /// Re-emit each value `delay` later.
    pub fn delay(&self, delay: Duration) -> Signal<T> {
        self.wrap(self.stream.delay(delay))
    }
}

impl Signal<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Signal<u64> {
        self.wrap(self.stream.count())
    }
}
