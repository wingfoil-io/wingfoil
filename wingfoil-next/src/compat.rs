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
use std::fmt::Debug;
use std::ops::{Not, Sub};
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::interp::Runner;
use crate::ops::EwmaDecay;
use crate::stats::StatisticsOps;

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

    /// Apply a fallible closure to each value; a returned `Err` aborts the run
    /// with context.
    pub fn try_map<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Result<B> + 'static,
    {
        self.wrap(self.stream.try_map(f))
    }

    /// Map and filter in one pass: `f` returns `(value, emit?)`.
    pub fn map_filter<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> (B, bool) + 'static,
    {
        self.wrap(self.stream.map_filter(f))
    }

    /// Combine with another signal; ticks when either input ticks.
    pub fn join<B, C, F>(&self, other: &Signal<B>, f: F) -> Signal<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        self.wrap(self.stream.join(&other.stream, f))
    }

    /// Combine with another signal read *passively*: this signal triggers the
    /// combine, `other`'s current value is read but does not trigger.
    pub fn join_passive<B, C, F>(&self, other: &Signal<B>, f: F) -> Signal<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        self.wrap(self.stream.join_passive(&other.stream, f))
    }

    /// Combine three signals (all active); ticks when any input ticks.
    pub fn join3<B, C, D, F>(&self, b: &Signal<B>, c: &Signal<C>, f: F) -> Signal<D>
    where
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&T, &B, &C) -> D + 'static,
    {
        self.wrap(self.stream.join3(&b.stream, &c.stream, f))
    }

    /// Combine with another signal via a *fallible* closure — the `try_`
    /// counterpart to [`join`](Signal::join). Both inputs active; a returned
    /// `Err` aborts the run with context.
    pub fn try_join<B, C, F>(&self, other: &Signal<B>, f: F) -> Signal<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> Result<C> + 'static,
    {
        self.wrap(self.stream.try_join(&other.stream, f))
    }

    /// [`join_passive`](Signal::join_passive) with a *fallible* closure: this
    /// signal triggers the combine, `other` is read passively, and a returned
    /// `Err` aborts the run with context.
    pub fn try_join_passive<B, C, F>(&self, other: &Signal<B>, f: F) -> Signal<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> Result<C> + 'static,
    {
        self.wrap(self.stream.try_join_passive(&other.stream, f))
    }

    /// Combine three signals (all active) via a *fallible* closure — the `try_`
    /// counterpart to [`join3`](Signal::join3). A returned `Err` aborts the run
    /// with context.
    pub fn try_join3<B, C, D, F>(&self, b: &Signal<B>, c: &Signal<C>, f: F) -> Signal<D>
    where
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&T, &B, &C) -> Result<D> + 'static,
    {
        self.wrap(self.stream.try_join3(&b.stream, &c.stream, f))
    }

    /// Run a side-effecting (fallible) closure on each tick — the graph's
    /// outbound edge (print, send, record). A returned `Err` aborts the run
    /// with context. Emits `()` per tick.
    pub fn for_each<F>(&self, f: F) -> Signal<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        self.wrap(self.stream.for_each(f))
    }

    /// Fan out into `n` parallel branches — each built by `branch` from a copy
    /// of this signal — and merge their outputs back into one signal (earliest-
    /// supplied ticked branch wins, as `merge`). `n` must be at least 1.
    ///
    /// The branch closure works in the classic idiom over [`Signal`], not the
    /// fluent [`Stream`] the underlying `fan` exposes; the wrapper threads the
    /// shared graph/runner into each branch signal and unwraps the result.
    pub fn fan<B, F>(&self, n: usize, branch: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(Signal<T>) -> Signal<B>,
    {
        let graph = self.graph.clone();
        let runner = self.runner.clone();
        let stream = self.stream.fan(n, |s: Stream<T>| {
            let sig = Signal {
                stream: s,
                graph: graph.clone(),
                runner: runner.clone(),
            };
            branch(sig).stream
        });
        self.wrap(stream)
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

    /// Observe each value with a side-effecting closure, passing it through
    /// unchanged (a debug tap).
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

    /// Chain the same endomorphic `map` `n` times. `map_n(0, f)` is the
    /// identity (a pass-through).
    pub fn map_n<F>(&self, n: usize, f: F) -> Signal<T>
    where
        F: Fn(&T) -> T + Clone + 'static,
    {
        self.wrap(self.stream.map_n(n, f))
    }

    /// Run `f` once at teardown — after the run ends, even if a cycle aborted
    /// it. Observes this signal's last value; emits nothing.
    pub fn finally<F>(&self, f: F) -> Signal<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        self.wrap(self.stream.finally(f))
    }
}

impl<T: Clone + Default + Debug + 'static> Signal<T> {
    /// Pass each value through unchanged while buffering it, then print the
    /// whole buffer (`{value:?}` per line) at teardown (the classic `print`).
    pub fn print(&self) -> Signal<T> {
        self.wrap(self.stream.print())
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

impl Signal<f64> {
    /// Exponentially-weighted moving average with an explicit [`EwmaDecay`]
    /// policy (per-tick alpha or clock half-life).
    pub fn ewma(&self, decay: EwmaDecay) -> Signal<f64> {
        self.wrap(self.stream.ewma(decay))
    }

    /// EWMA with a fixed smoothing factor `alpha` applied once per tick,
    /// seeded on the first sample.
    pub fn ewma_per_tick(&self, alpha: f64) -> Signal<f64> {
        self.wrap(self.stream.ewma_per_tick(alpha))
    }

    /// EWMA whose weights decay off engine time: a sample's weight halves
    /// every `half_life` of elapsed time, independent of tick rate.
    pub fn ewma_half_life(&self, half_life: Duration) -> Signal<f64> {
        self.wrap(self.stream.ewma_half_life(half_life))
    }

    /// Sum over a sliding window of the last `window` values.
    pub fn rolling_sum(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_sum(window))
    }

    /// Mean over a sliding window of the last `window` values.
    pub fn rolling_mean(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_mean(window))
    }

    /// Minimum over a sliding window of the last `window` values.
    pub fn rolling_min(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_min(window))
    }

    /// Maximum over a sliding window of the last `window` values.
    pub fn rolling_max(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_max(window))
    }

    /// Sample variance (ddof = 1) over a sliding window of the last `window`
    /// values.
    pub fn rolling_var(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_var(window))
    }

    /// Sample standard deviation over a sliding window of the last `window`
    /// values.
    pub fn rolling_std(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_std(window))
    }

    /// Median over a sliding window of the last `window` values (an even window
    /// averages its two middle values).
    pub fn rolling_median(&self, window: usize) -> Signal<f64> {
        self.wrap(self.stream.rolling_median(window))
    }
}
