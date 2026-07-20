//! Fluent wiring sugar: the classic wingfoil chaining style
//! (`ticker(d).count().map(f).filter(&cond)`) over the explicit
//! [`Builder`](crate::interp::Builder) core.
//!
//! A [`Stream<T>`] is a typed handle that also carries a shared reference to
//! the graph under construction, so combinators can be methods. This layer
//! is *wiring-time only* — it adds nothing to execution (the built
//! [`Runner`] is identical), and a future `graph!` macro or recording engine
//! sits at exactly this surface.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;

use crate::channel::ChannelSender;
use crate::interp::{AsHandle, Builder, ExternalSource, FeedbackSink, Handle, Runner};

/// A graph under construction. Cheap to clone; all clones share the same
/// underlying builder.
#[derive(Clone, Default)]
pub struct GraphBuilder {
    inner: Rc<RefCell<Builder>>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A source that ticks at a fixed interval.
    pub fn ticker(&self, period: Duration) -> Stream<()> {
        let handle = self.inner.borrow_mut().ticker(period);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// A source that ticks once with `value` on the first cycle.
    pub fn constant<T: Clone + Default + 'static>(&self, value: T) -> Stream<T> {
        let handle = self.inner.borrow_mut().constant(value);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// An external source: values sent through the returned
    /// [`ExternalSource`] (from any thread or async task) tick the stream.
    /// If several values arrive between cycles the latest wins. Graphs with
    /// external sources run in realtime mode only.
    pub fn external<T: Clone + Default + 'static>(&self) -> (Stream<T>, ExternalSource<T>) {
        let (handle, source) = self.inner.borrow_mut().external();
        (
            Stream {
                inner: self.inner.clone(),
                handle,
            },
            source,
        )
    }

    /// Mount a composite node (see [`Builder::composite`]). Used by the
    /// `graph!` macro's `nested` expansion; not intended to be called by
    /// hand.
    #[doc(hidden)]
    pub fn __composite<T: Clone + Default + 'static>(
        &self,
        active_ups: Vec<usize>,
        callback_activated: bool,
        node: impl FnMut(&mut crate::op::Ctx, bool) -> Result<crate::op::Tick<T>> + 'static,
    ) -> Stream<T> {
        let handle = self
            .inner
            .borrow_mut()
            .composite(active_ups, callback_activated, node);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// The shared per-cycle tick flags. Used by the `graph!` macro's
    /// `nested` expansion.
    #[doc(hidden)]
    pub fn __ticked(&self) -> Rc<RefCell<Vec<bool>>> {
        self.inner.borrow().ticked_rc()
    }

    /// A busy-poll source: `f` runs once per engine cycle, ticking on
    /// `Some`. Lossless and ordered — one value per cycle, no coalescing
    /// (contrast [`external`](Self::external), which collapses to
    /// latest-wins). The graph becomes a busy-spin loop: the kernel never
    /// parks. Realtime runs only.
    pub fn poll<T, F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static,
    {
        let handle = self.inner.borrow_mut().poll(f);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// Open a channel: a source stream fed by the returned [`ChannelSender`]
    /// (moved to another thread). Carries the message envelope, so a producer
    /// can also close the stream or propagate an error into the graph.
    /// Realtime runs only.
    pub fn channel<T: Clone + Default + 'static>(&self) -> (Stream<T>, ChannelSender<T>) {
        let (handle, sender) = self.inner.borrow_mut().channel::<T>();
        (
            Stream {
                inner: self.inner.clone(),
                handle,
            },
            sender,
        )
    }

    /// Open a feedback edge: a source stream (no upstreams — the graph stays
    /// acyclic) plus the [`FeedbackSink`] that feeds it. Close the loop with
    /// [`Stream::feedback`]; values arrive on the source one cycle later.
    pub fn feedback<T>(&self) -> (Stream<T>, FeedbackSink<T>)
    where
        T: Clone + Default + PartialEq + 'static,
    {
        let (handle, sink) = self.inner.borrow_mut().feedback::<T>();
        (
            Stream {
                inner: self.inner.clone(),
                handle,
            },
            sink,
        )
    }

    /// Consume the wired graph into a [`Runner`]. Streams stay usable as
    /// value handles (`runner.value(&stream)`); wiring further nodes from
    /// them afterwards is a logic error — they would target an empty builder.
    pub fn build(&self) -> Runner {
        std::mem::take(&mut *self.inner.borrow_mut()).build()
    }
}

/// A typed stream in a graph under construction. Combinators are methods, so
/// wiring chains: `g.ticker(p).count().map(|i| i * 2)`.
pub struct Stream<T> {
    inner: Rc<RefCell<Builder>>,
    handle: Handle<T>,
}

impl<T> Clone for Stream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            handle: self.handle,
        }
    }
}

impl<T> AsHandle<T> for Stream<T> {
    fn as_handle(&self) -> Handle<T> {
        self.handle
    }
}

impl<T> AsHandle<T> for &Stream<T> {
    fn as_handle(&self) -> Handle<T> {
        self.handle
    }
}

impl<T> Stream<T> {
    /// The underlying engine handle (for [`Runner::value`]).
    pub fn handle(&self) -> Handle<T> {
        self.handle
    }

    /// The shared value slot backing this stream. Used by the `graph!`
    /// macro's `nested` expansion to read composite inputs.
    #[doc(hidden)]
    pub fn __slot(&self) -> Rc<RefCell<T>>
    where
        T: 'static,
    {
        self.inner.borrow().slot(self.handle)
    }

    fn lift<B>(&self, handle: Handle<B>) -> Stream<B> {
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }
}

impl<T: 'static> Stream<T> {
    /// Apply a closure to each value.
    pub fn map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> B + 'static,
    {
        let h = self.inner.borrow_mut().map(self.handle, f);
        self.lift(h)
    }

    /// Apply a fallible closure to each value; a returned `Err` aborts the
    /// run with context.
    pub fn try_map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Result<B> + 'static,
    {
        let h = self.inner.borrow_mut().try_map(self.handle, f);
        self.lift(h)
    }

    /// Map and filter in one pass: `f` returns `(value, emit?)`.
    pub fn map_filter<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> (B, bool) + 'static,
    {
        let h = self.inner.borrow_mut().map_filter(self.handle, f);
        self.lift(h)
    }

    /// Pair each value with the current engine time: `(time, value)`.
    pub fn with_time(&self) -> Stream<(wingfoil::NanoTime, T)>
    where
        T: Clone,
    {
        let h = self.inner.borrow_mut().with_time(self.handle);
        self.lift(h)
    }

    /// Emit the current engine time whenever this stream ticks.
    pub fn ticked_at(&self) -> Stream<wingfoil::NanoTime> {
        let h = self.inner.borrow_mut().ticked_at(self.handle);
        self.lift(h)
    }

    /// Emit elapsed engine time (`now - start`) whenever this stream ticks.
    pub fn ticked_at_elapsed(&self) -> Stream<wingfoil::NanoTime> {
        let h = self.inner.borrow_mut().ticked_at_elapsed(self.handle);
        self.lift(h)
    }

    /// Fold values into an accumulator, emitting it after each fold.
    pub fn fold<B, F>(&self, init: B, f: F) -> Stream<B>
    where
        B: Clone + 'static,
        F: Fn(&mut B, &T) + 'static,
    {
        let h = self.inner.borrow_mut().fold(self.handle, init, f);
        self.lift(h)
    }

    /// Combine with another stream; ticks when either input ticks.
    pub fn join<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        let h = self.inner.borrow_mut().join(self.handle, other.handle, f);
        self.lift(h)
    }

    /// Combine with another stream read *passively*: this stream triggers the
    /// combine, `other`'s current value is read but does not trigger. The
    /// `bimap(Active, Passive)` of the classic engine — the shape a feedback
    /// input takes so the loop advances in step with the active source.
    pub fn join_passive<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        let h = self
            .inner
            .borrow_mut()
            .bimap(self.handle, true, other.handle, false, f);
        self.lift(h)
    }

    /// Combine three streams (all active); ticks when any input ticks.
    pub fn join3<B, C, D, F>(&self, b: &Stream<B>, c: &Stream<C>, f: F) -> Stream<D>
    where
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&T, &B, &C) -> D + 'static,
    {
        let h =
            self.inner
                .borrow_mut()
                .trimap(self.handle, true, b.handle, true, c.handle, true, f);
        self.lift(h)
    }
}

impl<T: Clone + Default + 'static> Stream<T> {
    /// Emit only when `condition`'s current value is true.
    pub fn filter(&self, condition: &Stream<bool>) -> Stream<T> {
        let h = self
            .inner
            .borrow_mut()
            .filter(self.handle, condition.handle);
        self.lift(h)
    }

    /// Emit the current value whenever `trigger` ticks (passive read).
    pub fn sample(&self, trigger: &Stream<()>) -> Stream<T> {
        let h = self.inner.borrow_mut().sample(self.handle, trigger.handle);
        self.lift(h)
    }

    /// Merge with another stream; the earliest-supplied ticked input wins.
    pub fn merge(&self, other: &Stream<T>) -> Stream<T> {
        let h = self.inner.borrow_mut().merge2(self.handle, other.handle);
        self.lift(h)
    }

    /// Pass through the first `limit` values, then stay quiet.
    pub fn limit(&self, limit: u32) -> Stream<T> {
        let h = self.inner.borrow_mut().limit(self.handle, limit);
        self.lift(h)
    }

    /// Rate-limit: emit at most once per `interval`.
    pub fn throttle(&self, interval: Duration) -> Stream<T> {
        let h = self.inner.borrow_mut().throttle(self.handle, interval);
        self.lift(h)
    }

    /// Buffer values and flush them as a `Vec` on each `interval` boundary
    /// (and once more on the last cycle).
    pub fn window(&self, interval: Duration) -> Stream<Vec<T>> {
        let h = self.inner.borrow_mut().window(self.handle, interval);
        self.lift(h)
    }

    /// Buffer values and flush them as a `Vec` once `capacity` accumulate
    /// (and once more on the last cycle).
    pub fn buffer(&self, capacity: usize) -> Stream<Vec<T>> {
        let h = self.inner.borrow_mut().buffer(self.handle, capacity);
        self.lift(h)
    }

    /// Observe each value with a side-effecting closure, passing it through
    /// unchanged (a debug tap).
    pub fn inspect<F>(&self, f: F) -> Stream<T>
    where
        F: Fn(&T) + 'static,
    {
        let h = self.inner.borrow_mut().inspect(self.handle, f);
        self.lift(h)
    }

    /// Collect every emitted value into a `Vec`.
    pub fn accumulate(&self) -> Stream<Vec<T>> {
        self.fold(Vec::new(), |acc, v: &T| acc.push(v.clone()))
    }
}

impl<T: Clone + Default + PartialEq + 'static> Stream<T> {
    /// Suppress consecutive duplicate values (emit on change only).
    pub fn distinct(&self) -> Stream<T> {
        let h = self.inner.borrow_mut().distinct(self.handle);
        self.lift(h)
    }
}

impl<T: Clone + Default + std::ops::Sub<Output = T> + 'static> Stream<T> {
    /// Emit the successive difference `value - previous`; quiet on the first.
    pub fn difference(&self) -> Stream<T> {
        let h = self.inner.borrow_mut().difference(self.handle);
        self.lift(h)
    }
}

impl<T: Clone + Default + std::ops::Not<Output = T> + 'static> Stream<T> {
    /// Negate each value (`!value`) — sugar over `map`.
    pub fn not(&self) -> Stream<T> {
        self.map(|v| !v.clone())
    }
}

impl Stream<f64> {
    /// Exponentially weighted moving average (statistics adapter).
    pub fn ewma(&self, decay: crate::ops::EwmaDecay) -> Stream<f64> {
        let h = self.inner.borrow_mut().ewma(self.handle, decay);
        self.lift(h)
    }

    /// EWMA with a fixed per-tick smoothing factor `alpha`.
    pub fn ewma_per_tick(&self, alpha: f64) -> Stream<f64> {
        self.ewma(crate::ops::EwmaDecay::PerTick(alpha))
    }

    /// EWMA decaying by `half_life` of elapsed engine time (tick-rate
    /// independent).
    pub fn ewma_half_life(&self, half_life: Duration) -> Stream<f64> {
        let hl = wingfoil::NanoTime::from(half_life);
        self.ewma(crate::ops::EwmaDecay::HalfLife(f64::from(hl)))
    }

    /// Rolling sum over the most recent `window` samples.
    pub fn rolling_sum(&self, window: usize) -> Stream<f64> {
        let h = self.inner.borrow_mut().rolling_sum(self.handle, window);
        self.lift(h)
    }

    /// Rolling arithmetic mean over the most recent `window` samples.
    pub fn rolling_mean(&self, window: usize) -> Stream<f64> {
        let h = self.inner.borrow_mut().rolling_mean(self.handle, window);
        self.lift(h)
    }
}

impl<T: 'static> Stream<T> {
    /// Run a side-effecting (fallible) closure on each tick — the graph's
    /// outbound edge (print, send, record). A returned `Err` aborts the run
    /// with context. Emits `()` per tick.
    pub fn for_each<F>(&self, f: F) -> Stream<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        let h = self.inner.borrow_mut().for_each(self.handle, f);
        self.lift(h)
    }
}

impl<T: Clone + Default + 'static> Stream<T> {
    /// Run `f` once at teardown — after the run ends, even if a cycle aborted
    /// it. Observes this stream's last value; emits nothing.
    pub fn finally<F>(&self, f: F) -> Stream<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        let h = self.inner.borrow_mut().finally(self.handle, f);
        self.lift(h)
    }
}

impl<T: Clone + Default + PartialEq + 'static> Stream<T> {
    /// Close a feedback loop: a pass-through of this stream that also sends
    /// each value to `sink`, to arrive on the paired source one cycle later.
    pub fn feedback(&self, sink: &FeedbackSink<T>) -> Stream<T> {
        let h = self.inner.borrow_mut().feedback_send(self.handle, sink);
        self.lift(h)
    }
}

impl<T: Clone + Default + PartialEq + 'static> Stream<T> {
    /// Re-emit each value `delay` later.
    pub fn delay(&self, delay: Duration) -> Stream<T> {
        let h = self.inner.borrow_mut().delay(self.handle, delay);
        self.lift(h)
    }
}

impl Stream<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Stream<u64> {
        self.fold(0u64, |acc, _| *acc += 1)
    }
}
