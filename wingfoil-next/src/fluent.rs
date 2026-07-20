//! Fluent wiring sugar: the classic wingfoil chaining style
//! (`ticker(d).count().map(f).filter(&cond)`) over the explicit
//! [`Builder`](crate::interp::Builder) core.
//!
//! Combinators are **extension traits**, not inherent methods, so the op set
//! is modular and open:
//!
//! - [`SourceOps`] — source constructors on [`GraphBuilder`]
//!   (`ticker`/`constant`/`external`/`channel`/`poll`/`feedback`);
//! - [`StreamOps`] — the core combinators on [`Stream<T>`] (`map`/`fold`/
//!   `filter`/`join`/…);
//! - [`StatisticsOps`](crate::stats::StatisticsOps) — a *separate* trait in
//!   `crate::stats`, brought into scope only when you want EWMA / rolling ops.
//!
//! Bring in what you need (`use wingfoil_next::prelude::*` for the common
//! two, plus any extra trait), and add your own: a third-party op trait just
//! implements methods over the public [`Stream::wire`] / [`GraphBuilder::source`]
//! extension primitives — the same way `StreamOps` and `StatisticsOps` do.
//!
//! This layer is *wiring-time only* — it adds nothing to execution (the built
//! [`Runner`] is identical).

use std::cell::RefCell;
use std::ops::{Not, Sub};
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use wingfoil::NanoTime;

use crate::burst::Burst;
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

    /// Extension point for source traits ([`SourceOps`] and third-party):
    /// wire a source from the builder and wrap the resulting handle as a
    /// [`Stream`]. Single-output sources are one-liners over this.
    pub fn source<T, F>(&self, f: F) -> Stream<T>
    where
        F: FnOnce(&mut Builder) -> Handle<T>,
    {
        let handle = f(&mut self.inner.borrow_mut());
        self.wrap(handle)
    }

    /// Run a closure with the underlying builder — for sources that return
    /// extra handles alongside the stream (external/channel/feedback).
    pub fn with_builder<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&mut Builder) -> R,
    {
        f(&mut self.inner.borrow_mut())
    }

    /// Wrap a handle from this builder as a [`Stream`].
    pub fn wrap<T>(&self, handle: Handle<T>) -> Stream<T> {
        Stream {
            inner: self.inner.clone(),
            handle,
        }
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
        self.wrap(handle)
    }

    /// The shared per-cycle tick flags. Used by the `graph!` macro's
    /// `nested` expansion.
    #[doc(hidden)]
    pub fn __ticked(&self) -> Rc<RefCell<Vec<bool>>> {
        self.inner.borrow().ticked_rc()
    }

    /// Consume the wired graph into a [`Runner`]. Streams stay usable as
    /// value handles (`runner.value(&stream)`); wiring further nodes from
    /// them afterwards is a logic error — they would target an empty builder.
    pub fn build(&self) -> Runner {
        std::mem::take(&mut *self.inner.borrow_mut()).build()
    }
}

/// Source constructors — the graph's entry points. An extension trait on
/// [`GraphBuilder`] so the source vocabulary is open the same way the
/// combinator vocabulary is.
pub trait SourceOps {
    /// A source that ticks at a fixed interval.
    fn ticker(&self, period: Duration) -> Stream<()>;

    /// A source that ticks once with `value` on the first cycle.
    fn constant<T: Clone + Default + 'static>(&self, value: T) -> Stream<T>;

    /// An external source: values sent through the returned [`ExternalSource`]
    /// (from any thread or async task) tick the stream. Emits a [`Burst`] of
    /// every value that arrived since the last cycle — never latest-wins.
    /// Realtime mode only.
    fn external<T: Clone + Default + 'static>(&self) -> (Stream<Burst<T>>, ExternalSource<T>);

    /// A channel source fed by the returned [`ChannelSender`] (moved to
    /// another thread). Emits a [`Burst`] (never latest-wins) and runs in
    /// **both** modes — realtime (waker-driven) and historical (timestamped
    /// sends replayed deterministically on the graph clock; see
    /// [`Builder::channel`](crate::interp::Builder::channel)).
    fn channel<T: Clone + Default + 'static>(&self) -> (Stream<Burst<T>>, ChannelSender<T>);

    /// A busy-poll source: `f` runs once per engine cycle, ticking on `Some`.
    /// Lossless and ordered — one value per cycle, no coalescing. The graph
    /// becomes a busy-spin loop: the kernel never parks. Realtime runs only.
    fn poll<T, F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static;

    /// Open a feedback edge: a source stream (no upstreams — the graph stays
    /// acyclic) plus the [`FeedbackSink`] that feeds it. Close the loop with
    /// [`StreamOps::feedback`]; values arrive on the source one cycle later.
    fn feedback<T>(&self) -> (Stream<T>, FeedbackSink<T>)
    where
        T: Clone + Default + PartialEq + 'static;
}

impl SourceOps for GraphBuilder {
    fn ticker(&self, period: Duration) -> Stream<()> {
        self.source(|b| b.ticker(period))
    }

    fn constant<T: Clone + Default + 'static>(&self, value: T) -> Stream<T> {
        self.source(|b| b.constant(value))
    }

    fn external<T: Clone + Default + 'static>(&self) -> (Stream<Burst<T>>, ExternalSource<T>) {
        let (handle, source) = self.with_builder(|b| b.external());
        (self.wrap(handle), source)
    }

    fn channel<T: Clone + Default + 'static>(&self) -> (Stream<Burst<T>>, ChannelSender<T>) {
        let (handle, sender) = self.with_builder(|b| b.channel::<T>());
        (self.wrap(handle), sender)
    }

    fn poll<T, F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static,
    {
        self.source(|b| b.poll(f))
    }

    fn feedback<T>(&self) -> (Stream<T>, FeedbackSink<T>)
    where
        T: Clone + Default + PartialEq + 'static,
    {
        let (handle, sink) = self.with_builder(|b| b.feedback::<T>());
        (self.wrap(handle), sink)
    }
}

/// A typed stream in a graph under construction. Combinators live in the
/// [`StreamOps`] extension trait (and others), so `use`ing the trait enables
/// chaining: `g.ticker(p).count().map(|i| i * 2)`.
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

    /// Extension point for combinator traits ([`StreamOps`],
    /// [`StatisticsOps`](crate::stats::StatisticsOps), and third-party op
    /// traits): run a wiring closure with the [`Builder`] and this stream's
    /// handle, wrapping the produced handle as a new stream. Every combinator
    /// is a one-liner over this — e.g. `self.wire(|b, h| b.map(h, f))` — so an
    /// op trait never touches the builder's internals.
    pub fn wire<B, F>(&self, f: F) -> Stream<B>
    where
        F: FnOnce(&mut Builder, Handle<T>) -> Handle<B>,
    {
        let handle = f(&mut self.inner.borrow_mut(), self.handle);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
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
}

/// The core stream combinators — an extension trait on [`Stream<T>`]. `use`
/// it (or `wingfoil_next::prelude::*`) to chain. Adapter-specific ops live in
/// their own traits (e.g. [`StatisticsOps`](crate::stats::StatisticsOps)).
pub trait StreamOps<T>: Sized {
    /// Apply a closure to each value.
    fn map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> B + 'static;

    /// Apply a fallible closure to each value; a returned `Err` aborts the
    /// run with context.
    fn try_map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Result<B> + 'static;

    /// Map and filter in one pass: `f` returns `(value, emit?)`.
    fn map_filter<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> (B, bool) + 'static;

    /// Pair each value with the current engine time: `(time, value)`.
    fn with_time(&self) -> Stream<(NanoTime, T)>
    where
        T: Clone + 'static;

    /// Emit the current engine time whenever this stream ticks.
    fn ticked_at(&self) -> Stream<NanoTime>
    where
        T: 'static;

    /// Emit elapsed engine time (`now - start`) whenever this stream ticks.
    fn ticked_at_elapsed(&self) -> Stream<NanoTime>
    where
        T: 'static;

    /// Fold values into an accumulator, emitting it after each fold.
    fn fold<B, F>(&self, init: B, f: F) -> Stream<B>
    where
        B: Clone + 'static,
        F: Fn(&mut B, &T) + 'static;

    /// Collect every emitted value into a `Vec`.
    fn accumulate(&self) -> Stream<Vec<T>>
    where
        T: Clone + Default + 'static;

    /// Combine with another stream; ticks when either input ticks.
    fn join<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static;

    /// Combine with another stream read *passively*: this stream triggers the
    /// combine, `other`'s current value is read but does not trigger — the
    /// `bimap(Active, Passive)` shape a feedback input takes.
    fn join_passive<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static;

    /// Combine three streams (all active); ticks when any input ticks.
    fn join3<B, C, D, F>(&self, b: &Stream<B>, c: &Stream<C>, f: F) -> Stream<D>
    where
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&T, &B, &C) -> D + 'static;

    /// Emit only when `condition`'s current value is true.
    fn filter(&self, condition: &Stream<bool>) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Emit the current value whenever `trigger` ticks (passive read).
    fn sample(&self, trigger: &Stream<()>) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Merge with another stream; the earliest-supplied ticked input wins.
    fn merge(&self, other: &Stream<T>) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Pass through the first `limit` values, then stay quiet.
    fn limit(&self, limit: u32) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Rate-limit: emit at most once per `interval`.
    fn throttle(&self, interval: Duration) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Buffer values and flush them as a `Vec` on each `interval` boundary
    /// (and once more on the last cycle).
    fn window(&self, interval: Duration) -> Stream<Vec<T>>
    where
        T: Clone + Default + 'static;

    /// Buffer values and flush them as a `Vec` once `capacity` accumulate
    /// (and once more on the last cycle).
    fn buffer(&self, capacity: usize) -> Stream<Vec<T>>
    where
        T: Clone + Default + 'static;

    /// Observe each value with a side-effecting closure, passing it through
    /// unchanged (a debug tap).
    fn inspect<F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn(&T) + 'static;

    /// Suppress consecutive duplicate values (emit on change only).
    fn distinct(&self) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static;

    /// Emit the successive difference `value - previous`; quiet on the first.
    fn difference(&self) -> Stream<T>
    where
        T: Clone + Default + Sub<Output = T> + 'static;

    /// Negate each value (`!value`) — sugar over `map`.
    fn not(&self) -> Stream<T>
    where
        T: Clone + Default + Not<Output = T> + 'static;

    /// Re-emit each value `delay` later.
    fn delay(&self, delay: Duration) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static;

    /// Run a side-effecting (fallible) closure on each tick — the graph's
    /// outbound edge (print, send, record). A returned `Err` aborts the run
    /// with context. Emits `()` per tick.
    fn for_each<F>(&self, f: F) -> Stream<()>
    where
        T: 'static,
        F: Fn(&T) -> Result<()> + 'static;

    /// Run `f` once at teardown — after the run ends, even if a cycle aborted
    /// it. Observes this stream's last value; emits nothing.
    fn finally<F>(&self, f: F) -> Stream<()>
    where
        T: Clone + Default + 'static,
        F: Fn(&T) -> Result<()> + 'static;

    /// Close a feedback loop: a pass-through of this stream that also sends
    /// each value to `sink`, to arrive on the paired source one cycle later.
    fn feedback(&self, sink: &FeedbackSink<T>) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static;
}

impl<T: 'static> StreamOps<T> for Stream<T> {
    fn map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> B + 'static,
    {
        self.wire(|b, h| b.map(h, f))
    }

    fn try_map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Result<B> + 'static,
    {
        self.wire(|b, h| b.try_map(h, f))
    }

    fn map_filter<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> (B, bool) + 'static,
    {
        self.wire(|b, h| b.map_filter(h, f))
    }

    fn with_time(&self) -> Stream<(NanoTime, T)>
    where
        T: Clone + 'static,
    {
        self.wire(|b, h| b.with_time(h))
    }

    fn ticked_at(&self) -> Stream<NanoTime> {
        self.wire(|b, h| b.ticked_at(h))
    }

    fn ticked_at_elapsed(&self) -> Stream<NanoTime> {
        self.wire(|b, h| b.ticked_at_elapsed(h))
    }

    fn fold<B, F>(&self, init: B, f: F) -> Stream<B>
    where
        B: Clone + 'static,
        F: Fn(&mut B, &T) + 'static,
    {
        self.wire(|b, h| b.fold(h, init, f))
    }

    fn accumulate(&self) -> Stream<Vec<T>>
    where
        T: Clone + Default + 'static,
    {
        self.fold(Vec::new(), |acc, v: &T| acc.push(v.clone()))
    }

    fn join<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        let other = other.handle();
        self.wire(|b, h| b.join(h, other, f))
    }

    fn join_passive<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        let other = other.handle();
        self.wire(|b, h| b.bimap(h, true, other, false, f))
    }

    fn join3<B, C, D, F>(&self, b: &Stream<B>, c: &Stream<C>, f: F) -> Stream<D>
    where
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&T, &B, &C) -> D + 'static,
    {
        let (bh, ch) = (b.handle(), c.handle());
        self.wire(|bld, h| bld.trimap(h, true, bh, true, ch, true, f))
    }

    fn filter(&self, condition: &Stream<bool>) -> Stream<T>
    where
        T: Clone + Default + 'static,
    {
        let cond = condition.handle();
        self.wire(|b, h| b.filter(h, cond))
    }

    fn sample(&self, trigger: &Stream<()>) -> Stream<T>
    where
        T: Clone + Default + 'static,
    {
        let trigger = trigger.handle();
        self.wire(|b, h| b.sample(h, trigger))
    }

    fn merge(&self, other: &Stream<T>) -> Stream<T>
    where
        T: Clone + Default + 'static,
    {
        let other = other.handle();
        self.wire(|b, h| b.merge2(h, other))
    }

    fn limit(&self, limit: u32) -> Stream<T>
    where
        T: Clone + Default + 'static,
    {
        self.wire(|b, h| b.limit(h, limit))
    }

    fn throttle(&self, interval: Duration) -> Stream<T>
    where
        T: Clone + Default + 'static,
    {
        self.wire(|b, h| b.throttle(h, interval))
    }

    fn window(&self, interval: Duration) -> Stream<Vec<T>>
    where
        T: Clone + Default + 'static,
    {
        self.wire(|b, h| b.window(h, interval))
    }

    fn buffer(&self, capacity: usize) -> Stream<Vec<T>>
    where
        T: Clone + Default + 'static,
    {
        self.wire(|b, h| b.buffer(h, capacity))
    }

    fn inspect<F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn(&T) + 'static,
    {
        self.wire(|b, h| b.inspect(h, f))
    }

    fn distinct(&self) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static,
    {
        self.wire(|b, h| b.distinct(h))
    }

    fn difference(&self) -> Stream<T>
    where
        T: Clone + Default + Sub<Output = T> + 'static,
    {
        self.wire(|b, h| b.difference(h))
    }

    fn not(&self) -> Stream<T>
    where
        T: Clone + Default + Not<Output = T> + 'static,
    {
        self.map(|v| !v.clone())
    }

    fn delay(&self, delay: Duration) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static,
    {
        self.wire(|b, h| b.delay(h, delay))
    }

    fn for_each<F>(&self, f: F) -> Stream<()>
    where
        T: 'static,
        F: Fn(&T) -> Result<()> + 'static,
    {
        self.wire(|b, h| b.for_each(h, f))
    }

    fn finally<F>(&self, f: F) -> Stream<()>
    where
        T: Clone + Default + 'static,
        F: Fn(&T) -> Result<()> + 'static,
    {
        self.wire(|b, h| b.finally(h, f))
    }

    fn feedback(&self, sink: &FeedbackSink<T>) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static,
    {
        self.wire(|b, h| b.feedback_send(h, sink))
    }
}

impl Stream<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Stream<u64> {
        self.fold(0u64, |acc, _| *acc += 1)
    }
}

impl<T: Clone + Default + 'static> Stream<Burst<T>> {
    /// Accumulate every value from every burst into one `Vec`, losslessly and
    /// in order — the burst-aware counterpart to
    /// [`accumulate`](StreamOps::accumulate).
    pub fn collapse_accumulate(&self) -> Stream<Vec<T>> {
        self.fold(Vec::new(), |acc, burst: &Burst<T>| {
            acc.extend(burst.iter().cloned())
        })
    }
}
