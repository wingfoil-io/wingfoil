//! Fluent wiring sugar: the legacy wingfoil chaining style
//! (`ticker(d).count().map(f).filter(&cond)`) over the explicit
//! [`Builder`] core.
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

use std::cell::{Cell, RefCell};
use std::fmt::Debug;
use std::ops::{Not, Sub};
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use wingfoil_next::{NanoTime, RunFor, RunMode};

use crate::Burst;
use crate::channel::ChannelSender;
use crate::interp::{
    AsHandle, Builder, ExternalSource, FeedbackSink, Handle, Runner, SlotRef, StopHandle,
};
use crate::op::{Activation, Ctx, Tick};

/// A graph under construction. Cheap to clone; all clones share the same
/// underlying builder.
///
/// [`build`](GraphBuilder::build) **consumes** the wired graph, so it may be
/// called only once: the `built` flag (shared with every [`Stream`] wired from
/// this builder) poisons the builder afterwards, turning a second `build()` —
/// or any wiring from a retained `Stream` — into an explicit panic rather than
/// the silent empty-`Runner` / out-of-bounds-`slot()` footgun it would
/// otherwise be.
#[derive(Clone, Default)]
pub struct GraphBuilder {
    inner: Rc<RefCell<Builder>>,
    built: Rc<Cell<bool>>,
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
        self.assert_not_built();
        let handle = f(&mut self.inner.borrow_mut());
        self.wrap(handle)
    }

    /// Panic (with an explanatory message) if this builder has already been
    /// consumed by [`build`](GraphBuilder::build). Wiring after `build` would
    /// target an empty builder and later panic out-of-bounds inside `slot()`;
    /// failing loudly here names the precondition instead.
    fn assert_not_built(&self) {
        assert!(
            !self.built.get(),
            "invariant: GraphBuilder already consumed by build(); wire all \
             nodes before calling build() (build() may be called only once)"
        );
    }

    /// Run a closure with the underlying builder — for sources that return
    /// extra handles alongside the stream (external/channel/feedback).
    pub fn with_builder<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&mut Builder) -> R,
    {
        self.assert_not_built();
        f(&mut self.inner.borrow_mut())
    }

    /// Resolve the tokio runtime handle the async adapters spawn onto: the
    /// caller override installed via [`with_async_runtime`](Self::with_async_runtime)
    /// if any, otherwise a runtime this graph creates lazily and owns for its
    /// lifetime (one per graph — every async adapter shares it, dropped at
    /// teardown). Used by [`produce_async`](crate::async_source::produce_async)
    /// and [`consume_async`](crate::async_source::consume_async); adapters rarely
    /// call it directly. See `docs/runtime-ownership.md`.
    #[cfg(feature = "async")]
    pub fn async_runtime_handle(&self) -> Result<tokio::runtime::Handle> {
        self.assert_not_built();
        self.inner.borrow_mut().async_runtime_handle()
    }

    /// Embed the graph's async adapters in a caller-supplied runtime instead of
    /// the graph's own. The override escape hatch (see `docs/runtime-ownership.md`):
    /// by default a graph lazily creates and owns one runtime shared by all its
    /// async adapters; call this to point them at your existing runtime for
    /// embedding in an async application or a custom runtime configuration.
    ///
    /// ```ignore
    /// let rt = tokio::runtime::Runtime::new()?;
    /// let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());
    /// ```
    #[cfg(feature = "async")]
    pub fn with_async_runtime(self, handle: tokio::runtime::Handle) -> Self {
        self.inner.borrow_mut().set_async_runtime_override(handle);
        self
    }

    /// Wrap a handle from this builder as a [`Stream`].
    pub fn wrap<T>(&self, handle: Handle<T>) -> Stream<T> {
        Stream {
            inner: self.inner.clone(),
            built: self.built.clone(),
            handle,
        }
    }

    /// Mount a composite node (see [`Builder::composite`]). Used by the
    /// `nitro!` macro's `nested` expansion; not intended to be called by
    /// hand.
    #[doc(hidden)]
    pub fn __composite<T: Clone + Default + 'static>(
        &self,
        active_ups: Vec<usize>,
        passive_ups: Vec<usize>,
        callback_activated: bool,
        node: impl FnMut(&mut crate::op::Ctx, crate::op::CompositePhase) -> Result<crate::op::Tick<T>>
        + 'static,
    ) -> Stream<T> {
        self.assert_not_built();
        let handle =
            self.inner
                .borrow_mut()
                .composite(active_ups, passive_ups, callback_activated, node);
        self.wrap(handle)
    }

    /// The shared per-cycle tick flags. Used by the `nitro!` macro's
    /// `nested` expansion.
    #[doc(hidden)]
    pub fn __ticked(&self) -> Rc<RefCell<Vec<bool>>> {
        self.inner.borrow().ticked_rc()
    }

    /// Consume the wired graph into a [`Runner`]. Streams stay usable as
    /// value handles (`runner.value(&stream)`).
    ///
    /// Also available at the end of a chain as [`Stream::build`], which builds
    /// this same graph — use it when nothing needs reading back and the whole
    /// program can be one expression.
    ///
    /// # Precondition
    ///
    /// May be called **once**: it takes the underlying [`Builder`], leaving the
    /// `GraphBuilder` (and every clone) empty. A second `build()` — or wiring a
    /// further node from a retained [`Stream`] afterwards — panics with an
    /// explanatory message rather than silently returning an empty [`Runner`]
    /// (the previous footgun, which later panicked out-of-bounds deep inside
    /// `slot()`).
    pub fn build(&self) -> Runner {
        assert!(
            !self.built.replace(true),
            "invariant: GraphBuilder::build() called twice; it consumes the \
             wired graph, so a second call would return an empty Runner"
        );
        std::mem::take(&mut *self.inner.borrow_mut()).build()
    }

    /// Combine several same-type streams into a `Stream<Burst<T>>` (the legacy
    /// `combine`): each cycle gathers the current values of every input that
    /// ticked *this* instant into one [`Burst`], in argument order — same-instant
    /// values ride one burst, never latest-wins.
    pub fn combine<T: Clone + Default + 'static>(&self, streams: &[Stream<T>]) -> Stream<Burst<T>> {
        let handles: Vec<Handle<T>> = streams.iter().map(|s| s.handle()).collect();
        let handle = self.with_builder(|b| b.combine(&handles));
        self.wrap(handle)
    }

    /// Wire a **custom node** — the public extension point for a caller-driven
    /// graph node (the next equivalent of legacy `MutableNode` +
    /// `StreamPeekRef`). The node is activated by its `active` upstreams' ticks,
    /// reads any upstream's `passive` value without being triggered by it, and
    /// runs `cycle` each activation to produce a [`Tick<T>`].
    ///
    /// Upstreams are declared type-erased ([`Upstream`]), so inputs of different
    /// value types mix freely — only their node indices matter for dispatch. To
    /// read an upstream's value inside `cycle`, capture its
    /// [`SlotRef`](crate::interp::SlotRef) via [`Stream::value_slot`] at wiring
    /// time and `borrow()` it. `activation` declares scheduling behaviour
    /// ([`Activation::NONE`] for tick-driven, [`Activation::SCHEDULES`] for
    /// self-scheduling via [`Ctx::schedule`], …).
    ///
    /// A graph containing a custom node is single-run in v1 (caller-owned state
    /// has no engine reset hook — see [`Builder::custom_node`]).
    pub fn custom_node<T, F>(
        &self,
        active: &[Upstream],
        passive: &[Upstream],
        activation: Activation,
        cycle: F,
    ) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: FnMut(&mut Ctx<'_>) -> Result<Tick<T>> + 'static,
    {
        let active_ups: Vec<usize> = active.iter().map(|u| u.idx).collect();
        let passive_ups: Vec<usize> = passive.iter().map(|u| u.idx).collect();
        let handle =
            self.with_builder(|b| b.custom_node(active_ups, passive_ups, activation, cycle));
        self.wrap(handle)
    }

    /// Queue a finite, non-decreasing timestamped sequence onto a
    /// [`channel`](SourceOps::channel) source and close it, returning the
    /// historical-replay [`Burst`] source. Row `(value, time)` is delivered on
    /// the graph clock at `time` (records sharing a timestamp ride one atomic
    /// burst); the sender is closed once the sequence is exhausted so the
    /// historical receiver stops collecting.
    ///
    /// Rows are `Result`, so a mid-sequence error is forwarded through
    /// [`ChannelSender::send_error`](crate::channel::ChannelSender::send_error)
    /// — aborting the run with that error's context — and iteration stops
    /// there. This is the shared engine of the
    /// [`replay_lines`](crate::adapters::lines::replay_lines) /
    /// [`csv_read`](crate::adapters::csv::csv_read) sources; `csv_read` relies
    /// on the error-then-stop shape to surface a decode failure.
    pub fn replay_results<T, I>(&self, rows: I) -> Stream<Burst<T>>
    where
        T: Clone + Default + 'static,
        I: IntoIterator<Item = Result<(T, NanoTime)>>,
    {
        let (stream, sender) = self.channel::<T>();
        for row in rows {
            match row {
                Ok((value, time)) => {
                    sender.send_at(value, time);
                }
                // Mirror the legacy adapters: forward the producer error into
                // the graph (it aborts the run with context) and stop queueing.
                Err(error) => {
                    sender.send_error(error);
                    break;
                }
            }
        }
        // Queue the end-of-stream so the historical receiver stops collecting.
        sender.close();
        stream
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

    /// [`channel`](Self::channel) with an optional transport bound. `None` is the
    /// unbounded default; `Some(n)` makes the producer→graph transport a bounded
    /// `sync_channel(n)`, so a producer sending faster than the graph drains
    /// **blocks** on `send` (back-pressure) instead of queueing an unbounded
    /// backlog. **Do not** bound a producer that fills the buffer *before the run
    /// starts* (e.g. a `replay_results` feed queued at wiring — with no running
    /// consumer to drain it, a bounded send blocks at wiring); use plain
    /// [`channel`](Self::channel) there.
    fn channel_bounded<T: Clone + Default + 'static>(
        &self,
        buffer: Option<usize>,
    ) -> (Stream<Burst<T>>, ChannelSender<T>);

    /// A busy-poll source: `f` runs once per engine cycle, ticking on `Some`.
    /// Lossless and ordered — one value per cycle, no coalescing. The graph
    /// becomes a busy-spin loop: the kernel never parks. Realtime runs only.
    fn poll<T, F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static;

    /// A [`channel`](SourceOps::channel)-fed source whose producer (thread /
    /// socket) is established in `start()` rather than at wiring — the
    /// **deferred-connection** primitive for I/O adapters. `setup` runs on each
    /// run start, is handed the [`ChannelSender`], connects/spawns the live
    /// producer, and returns a [`StopHandle`] dropped at teardown to stop it.
    /// The factory itself does **no** I/O, so adapter wiring (address parse,
    /// registry lookup, historical-mode rejection) stays pure and unit-testable;
    /// a `setup` error aborts the run at start with node context. See
    /// [`Builder::source_at_start`](crate::interp::Builder::source_at_start).
    fn source_at_start<T, Setup>(&self, setup: Setup) -> Stream<Burst<T>>
    where
        T: Clone + Default + 'static,
        Setup: FnMut(ChannelSender<T>) -> Result<StopHandle> + 'static;

    /// Open a feedback edge: a source stream (no upstreams — the graph stays
    /// acyclic) plus the [`FeedbackSink`] that feeds it. Close the loop with
    /// [`StreamOps::feedback`]; values arrive on the source one cycle later.
    fn feedback<T>(&self) -> (Stream<T>, FeedbackSink<T>)
    where
        T: Clone + Default + PartialEq + 'static;

    /// A source that never ticks (the legacy `never`). Useful as an inert
    /// trigger — e.g. a [`delay_with_reset`](StreamOps::delay_with_reset) that
    /// never resets behaves like a plain [`delay`](StreamOps::delay).
    fn never(&self) -> Stream<()>;

    /// Run a producer sub-graph on its own worker thread and surface its output
    /// here as a [`Burst`] stream.
    ///
    /// `build` runs **on the worker thread** at run start: it wires a fresh graph
    /// with the [`GraphBuilder`] it is handed and returns the stream to forward.
    /// The worker runs that graph under the *same* run mode and bound as this one
    /// and sends each value back over the channel layer — timestamped, so a
    /// historical replay stays deterministic (one value per instant, grouped into
    /// the delivered burst). The worker thread is joined at teardown; a worker
    /// error aborts this run.
    ///
    /// next's ergonomic twin of legacy `producer()` — the thread-offload half of
    /// `graph_node`. It wraps the channel + `send_at` + `close` + join plumbing
    /// the `threading` example otherwise spells out by hand.
    fn spawn<U, F>(&self, build: F) -> Stream<Burst<U>>
    where
        U: Clone + Default + Send + 'static,
        F: FnOnce(&GraphBuilder) -> Stream<U> + Send + 'static;

    /// [`spawn`](Self::spawn) with an optional bound on the worker→graph channel:
    /// `None` unbounded (default), `Some(n)` back-pressures the worker (it blocks
    /// producing once `n` values are queued unread, in realtime — historical
    /// drains in lock-step regardless).
    fn spawn_bounded<U, F>(&self, buffer: Option<usize>, build: F) -> Stream<Burst<U>>
    where
        U: Clone + Default + Send + 'static,
        F: FnOnce(&GraphBuilder) -> Stream<U> + Send + 'static;
}

/// Joins a [`SourceOps::spawn`] worker thread when the run tears down — held only
/// for its `Drop` by a [`StopHandle`]. The `spawn` worker is a self-bounded
/// producer (it ends at its inherited run bound), so a plain join suffices.
struct JoinOnDrop(Option<std::thread::JoinHandle<()>>);

impl Drop for JoinOnDrop {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            let _ = h.join();
        }
    }
}

/// Tears down a [`StreamOps::spawn_map`] worker: closes its **input** so the
/// lock-step reader sees end-of-stream and its run ends, *then* joins. Without the
/// close the worker would block forever awaiting an input the driving graph has
/// stopped sending (e.g. after a `limit`), and the join would hang.
struct WorkerGuard<I> {
    input: Rc<RefCell<Option<ChannelSender<I>>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl<I> Drop for WorkerGuard<I> {
    fn drop(&mut self) {
        if let Some(s) = self.input.borrow().as_ref() {
            s.close();
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

/// The worker-thread body of [`SourceOps::spawn`]: wire the producer sub-graph,
/// forward each timestamped value over `sender`, run under the caller's bound,
/// and close (or surface a worker error) on the way out.
fn run_worker_source<U, F>(build: F, sender: ChannelSender<U>, run_mode: RunMode, run_for: RunFor)
where
    U: Clone + Default + Send + 'static,
    F: FnOnce(&GraphBuilder) -> Stream<U>,
{
    let wg = GraphBuilder::new();
    let out = build(&wg);
    let forward = sender.clone();
    let _sink = out.with_time().for_each(move |tv: &(NanoTime, U)| {
        // send_at replays deterministically at graph time in historical mode and
        // is treated as a plain send (stamp ignored) in realtime.
        forward.send_at(tv.1.clone(), tv.0);
        Ok(())
    });
    let mut runner = wg.build();
    match runner.run(run_mode, run_for) {
        // Signal end-of-stream so the receiving graph winds down once drained.
        Ok(()) => {
            sender.close();
        }
        // Propagate the worker's failure into the driving graph, aborting its run.
        Err(e) => {
            sender.send_error(e);
        }
    }
}

/// The worker-thread body of [`StreamOps::spawn_map`]: build a graph whose source
/// is a lock-step input channel (fed by the driving graph), run `build` over it,
/// forward each timestamped result back over `to_main`, and hand the input sender
/// back to the driving graph over `sender_back` so it can start feeding.
fn run_worker_map<I, O, F>(
    build: F,
    to_main: ChannelSender<O>,
    sender_back: std::sync::mpsc::Sender<ChannelSender<I>>,
    buffer: Option<usize>,
    run_mode: RunMode,
    run_for: RunFor,
) where
    I: Clone + Default + Send + 'static,
    O: Clone + Default + Send + 'static,
    F: FnOnce(Stream<Burst<I>>) -> Stream<O>,
{
    let wg = GraphBuilder::new();
    // Lock-step input: one value per instant, no read-ahead, so the worker never
    // blocks on input the driving graph cannot send while it awaits this output.
    let (in_handle, sender_in) = wg.with_builder(|b| b.channel_lockstep::<I>(buffer));
    let in_stream: Stream<Burst<I>> = wg.wrap(in_handle);
    // Hand the input sender back; if the driving graph has gone, there is nothing
    // to run.
    if sender_back.send(sender_in).is_err() {
        return;
    }
    let out = build(in_stream);
    let forward = to_main.clone();
    let _sink = out.with_time().for_each(move |tv: &(NanoTime, O)| {
        forward.send_at(tv.1.clone(), tv.0);
        Ok(())
    });
    let mut runner = wg.build();
    match runner.run(run_mode, run_for) {
        Ok(()) => {
            to_main.close();
        }
        Err(e) => {
            to_main.send_error(e);
        }
    }
}

impl SourceOps for GraphBuilder {
    __wf_fluent_ticker!();

    __wf_fluent_constant!();

    fn external<T: Clone + Default + 'static>(&self) -> (Stream<Burst<T>>, ExternalSource<T>) {
        let (handle, source) = self.with_builder(|b| b.external());
        (self.wrap(handle), source)
    }

    fn channel<T: Clone + Default + 'static>(&self) -> (Stream<Burst<T>>, ChannelSender<T>) {
        self.channel_bounded(None)
    }

    fn channel_bounded<T: Clone + Default + 'static>(
        &self,
        buffer: Option<usize>,
    ) -> (Stream<Burst<T>>, ChannelSender<T>) {
        let (handle, sender) = self.with_builder(|b| b.channel_bounded::<T>(buffer));
        (self.wrap(handle), sender)
    }

    fn poll<T, F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static,
    {
        self.source(|b| b.poll(f))
    }

    fn source_at_start<T, Setup>(&self, setup: Setup) -> Stream<Burst<T>>
    where
        T: Clone + Default + 'static,
        Setup: FnMut(ChannelSender<T>) -> Result<StopHandle> + 'static,
    {
        let handle = self.with_builder(|b| b.source_at_start(setup));
        self.wrap(handle)
    }

    fn feedback<T>(&self) -> (Stream<T>, FeedbackSink<T>)
    where
        T: Clone + Default + PartialEq + 'static,
    {
        let (handle, sink) = self.with_builder(|b| b.feedback::<T>());
        (self.wrap(handle), sink)
    }

    fn never(&self) -> Stream<()> {
        self.source(|b| b.never())
    }

    fn spawn<U, F>(&self, build: F) -> Stream<Burst<U>>
    where
        U: Clone + Default + Send + 'static,
        F: FnOnce(&GraphBuilder) -> Stream<U> + Send + 'static,
    {
        self.spawn_bounded(None, build)
    }

    fn spawn_bounded<U, F>(&self, buffer: Option<usize>, build: F) -> Stream<Burst<U>>
    where
        U: Clone + Default + Send + 'static,
        F: FnOnce(&GraphBuilder) -> Stream<U> + Send + 'static,
    {
        // `build` runs once, on the worker thread, at run start; wrap it so the
        // `FnMut` setup can move it out (the backing channel source is single-run,
        // so `setup` fires exactly once). The `Rc` stays on this thread; only the
        // moved-out `F` (which is `Send`) crosses to the worker.
        let build = Rc::new(RefCell::new(Some(build)));
        let handle = self.with_builder(move |b| {
            b.source_at_start_with_params(buffer, move |sender, run_mode, run_for, _start_time| {
                let build = build.borrow_mut().take().expect(
                    "invariant: spawn worker built once per run (backing channel is single-run)",
                );
                let worker = std::thread::spawn(move || {
                    run_worker_source(build, sender, run_mode, run_for);
                });
                Ok(StopHandle::new(JoinOnDrop(Some(worker))))
            })
        });
        self.wrap(handle)
    }
}

/// A typed stream in a graph under construction. Combinators live in the
/// [`StreamOps`] extension trait (and others), so `use`ing the trait enables
/// chaining: `g.ticker(p).count().map(|i| i * 2)`. [`build`](Stream::build)
/// closes the chain, so wiring, building and running can be one expression.
pub struct Stream<T> {
    inner: Rc<RefCell<Builder>>,
    /// Shared with the owning [`GraphBuilder`]: set once the graph is built, so
    /// wiring from a retained `Stream` afterwards fails loudly (see
    /// [`Stream::wire`]).
    built: Rc<Cell<bool>>,
    handle: Handle<T>,
}

impl<T> Clone for Stream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            built: self.built.clone(),
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

    /// A [`GraphBuilder`] view onto the same graph this stream belongs to.
    /// Sink combinators (e.g. the async adapter `*_pub`/`*_write` traits) use it
    /// to reach builder-level services such as the graph-owned async runtime,
    /// since they receive `self: Stream`, not the builder. Cheap — clones the two
    /// shared `Rc`s.
    pub fn graph(&self) -> GraphBuilder {
        GraphBuilder {
            inner: self.inner.clone(),
            built: self.built.clone(),
        }
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
        assert!(
            !self.built.get(),
            "invariant: wiring a Stream after GraphBuilder::build(); the graph \
             is already consumed. Wire all nodes before calling build()"
        );
        let handle = f(&mut self.inner.borrow_mut(), self.handle);
        Stream {
            inner: self.inner.clone(),
            built: self.built.clone(),
            handle,
        }
    }

    /// The shared value slot backing this stream. Used by the `nitro!`
    /// macro's `nested` expansion to read composite inputs. Returns a
    /// [`SlotRef`] — the frozen access boundary — not the concrete cell, so the
    /// value store can move to an arena later without changing this hook.
    #[doc(hidden)]
    pub fn __slot(&self) -> SlotRef<T>
    where
        T: 'static,
    {
        self.inner.borrow().slot(self.handle)
    }

    /// The value slot backing this stream, for reading its current value from
    /// inside a [`custom_node`](GraphBuilder::custom_node) cycle closure.
    ///
    /// Capture the returned [`SlotRef`] at wiring time and `borrow()` it each
    /// cycle to read the upstream's current value — this is how a custom node
    /// reaches its upstreams, the next analogue of a legacy `MutableNode`
    /// reading the `Rc<dyn Stream>`s it holds. The scheduler's single-fire,
    /// layer-ordered dispatch guarantees the upstream has already written its
    /// slot this cycle before the custom node reads it.
    pub fn value_slot(&self) -> SlotRef<T>
    where
        T: 'static,
    {
        self.inner.borrow().slot(self.handle)
    }

    /// Consume the wired graph this stream belongs to into a [`Runner`] —
    /// [`GraphBuilder::build`] reached from the end of a chain, so a whole
    /// program can be one expression:
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use wingfoil_next::prelude::*;
    /// use wingfoil_next::{RunFor, RunMode};
    ///
    /// GraphBuilder::new()
    ///     .ticker(Duration::from_secs(1))
    ///     .count()
    ///     .map(|i| format!("hello, world {i}"))
    ///     .print()
    ///     .build()
    ///     .run(RunMode::RealTime, RunFor::Cycles(3))
    ///     .unwrap();
    /// ```
    ///
    /// It builds the *graph*, not this stream: which stream you call it on
    /// makes no difference, and every other stream wired from the same
    /// [`GraphBuilder`] stays usable as a value handle
    /// (`runner.value(&stream)`). Carries the same **call-once** precondition
    /// as [`GraphBuilder::build`] — a second `build()`, from any stream or the
    /// builder itself, panics rather than returning an empty [`Runner`]. When
    /// you need to read values back, keep the builder and the streams in
    /// bindings instead of chaining.
    pub fn build(&self) -> Runner {
        self.graph().build()
    }

    /// Erase this stream to an [`Upstream`] edge for
    /// [`custom_node`](GraphBuilder::custom_node). A custom node's upstreams may
    /// have different value types, so they are declared type-erased; only the
    /// node index matters for dispatch.
    pub fn upstream(&self) -> Upstream {
        Upstream {
            idx: self.handle.index(),
        }
    }
}

/// A type-erased reference to an already-wired node, used to declare a
/// [`custom_node`](GraphBuilder::custom_node)'s upstream edges regardless of
/// the upstreams' value types (only the node index matters for scheduling).
/// Build one with [`Stream::upstream`] or the `From<&Stream<T>>` impl.
#[derive(Clone, Copy)]
pub struct Upstream {
    idx: usize,
}

impl<T> From<&Stream<T>> for Upstream {
    fn from(stream: &Stream<T>) -> Self {
        stream.upstream()
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

    /// Combine with another stream via a *fallible* closure — the `try_`
    /// counterpart to [`join`](StreamOps::join). Both inputs active; a returned
    /// `Err` aborts the run with context (the legacy `try_bimap`).
    fn try_join<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> Result<C> + 'static;

    /// [`join_passive`](StreamOps::join_passive) with a *fallible* closure:
    /// this stream triggers the combine, `other` is read passively, and a
    /// returned `Err` aborts the run with context.
    fn try_join_passive<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> Result<C> + 'static;

    /// Combine three streams (all active) via a *fallible* closure — the
    /// `try_` counterpart to [`join3`](StreamOps::join3). A returned `Err`
    /// aborts the run with context (the legacy `try_trimap`).
    fn try_join3<B, C, D, F>(&self, b: &Stream<B>, c: &Stream<C>, f: F) -> Stream<D>
    where
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&T, &B, &C) -> Result<D> + 'static;

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

    /// Merge this stream with every stream in `others` into one — the n-ary
    /// `merge`. On a same-cycle tie the earliest-supplied ticked input wins:
    /// this stream first, then `others` in slice order. `merge_all(&[])` is
    /// just this stream.
    ///
    /// Wires a **single** n-ary [`MergeN`](crate::ops::MergeN) node — legacy
    /// `merge(vec)`'s twin — not a left-associated chain of 2-ary
    /// [`merge`](StreamOps::merge)s. The two are semantically identical (2-ary
    /// merge's earliest-wins tie-break is associative), but a chain costs
    /// `n - 1` extra nodes and `n - 1` extra depth, which measured 1.86x
    /// legacy on a busy 256-wide fan-in; see [`MergeN`](crate::ops::MergeN).
    fn merge_all(&self, others: &[&Stream<T>]) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Chain the same endomorphic `map` `n` times. `map_n(0, f)` is the
    /// identity (a pass-through). Bounded repetition sugar for a straight deep
    /// chain; inside `nitro!` the count must be a literal so the DAG stays
    /// static.
    fn map_n<F>(&self, n: usize, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn(&T) -> T + Clone + 'static;

    /// Fan out into `n` parallel branches — each built by `branch` from a copy
    /// of this stream — and merge their outputs back into one stream (earliest-
    /// supplied ticked branch wins, as `merge`). `n` must be at least 1. Inside
    /// `nitro!` the count must be a literal so the DAG stays static.
    ///
    /// The fan-in is one n-ary [`merge_all`](StreamOps::merge_all) node, so a
    /// 256-way fan costs 256 branch tails plus **one** merge — not the
    /// 255-node, 255-deep binary chain it used to build.
    fn fan<B, F>(&self, n: usize, branch: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(Stream<T>) -> Stream<B>;

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

    /// Log each value as it ticks — `"{time} {label} {value:?}"` at `level`,
    /// through the `log` crate (target `"wingfoil"`) — passing it through
    /// unchanged (the legacy `logged` debug tap). Wire up any `log`-compatible
    /// backend (e.g. `env_logger`) to see the output.
    fn logged(&self, label: &str, level: log::Level) -> Stream<T>
    where
        T: Clone + Default + Debug + 'static;

    /// Suppress consecutive duplicate values (emit on change only).
    fn distinct(&self) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static;

    /// Suppress ticks while the change from the **last emitted** value is
    /// small: `is_small(current, last_emitted)` returning `true` drops the
    /// tick. The first value always ticks; the reference is the last value
    /// emitted, not the last seen, so a slow drift still eventually ticks.
    fn drop_small_change<F>(&self, is_small: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn(&T, &T) -> bool + 'static;

    /// Emit the successive difference `value - previous`; quiet on the first.
    fn difference(&self) -> Stream<T>
    where
        T: Clone + Default + Sub<Output = T> + 'static;

    /// Negate each value (`!value`) — sugar over `map`.
    fn not(&self) -> Stream<T>
    where
        T: Clone + Default + Not<Output = T> + 'static;

    /// Pass each value through unchanged, printing it (`{value:?}` per line) to
    /// stdout as it ticks (the legacy `print`). Unlike legacy, which buffers
    /// and dumps at teardown, next prints per-tick — a justified deviation (see
    /// the [`Print`](crate::ops::Print) op docs).
    fn print(&self) -> Stream<T>
    where
        T: Clone + Default + Debug + 'static;

    /// Pass each value through unchanged, printing a performance summary at
    /// the end of the run (the legacy `timed`).
    fn timed(&self) -> Stream<T>
    where
        T: Clone + Default + 'static;

    /// Re-emit each value `delay` later.
    fn delay(&self, delay: Duration) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static;

    /// [`delay`](StreamOps::delay) with a reset trigger (the legacy
    /// `delay_with_reset`): when `trigger` ticks, the output snaps to the
    /// current value and any pending (delayed) values are dropped. `trigger`
    /// is read for its tick only, so its value type is irrelevant.
    fn delay_with_reset<U>(&self, delay: Duration, trigger: &Stream<U>) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static,
        U: 'static;

    /// Collapse a burst/iterator value into a single tick of its **last** item
    /// (the legacy `collapse`); stays quiet when the iterator is empty. Sugar
    /// over [`map_filter`](StreamOps::map_filter).
    fn collapse<OUT>(&self) -> Stream<OUT>
    where
        T: Clone + IntoIterator<Item = OUT> + 'static,
        OUT: Clone + Default + 'static;

    /// Run a side-effecting (fallible) closure on each tick — the graph's
    /// outbound edge (print, send, record). A returned `Err` aborts the run
    /// with context. Emits `()` per tick.
    fn for_each<F>(&self, f: F) -> Stream<()>
    where
        T: 'static,
        F: Fn(&T) -> Result<()> + 'static;

    /// Side-effecting sink over an owned, mutable resource opened at wiring
    /// time — the `&mut` counterpart to [`for_each`](StreamOps::for_each).
    /// `writer` is moved in and wrapped in a `RefCell`; `f` runs once per tick
    /// with a `&mut W` to it, and a returned `Err` aborts the run with context.
    /// Spares every file/socket sink the "`for_each` takes an `Fn` but my
    /// writer needs `&mut`, so wrap it in a `RefCell`" boilerplate. Emits `()`
    /// per tick.
    fn for_each_mut<W, F>(&self, writer: W, f: F) -> Stream<()>
    where
        T: 'static,
        W: 'static,
        F: Fn(&mut W, &T) -> Result<()> + 'static;

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

    /// Map this stream through a sub-graph running on its own worker thread,
    /// surfacing the result here as a [`Burst`] stream.
    ///
    /// `build` runs **on the worker thread** at run start: it is handed a
    /// [`Burst`] stream of this stream's values (delivered into the worker over
    /// the channel layer) and returns the stream to send back. The worker runs
    /// under the *same* run mode and bound as this graph and forwards each result
    /// — timestamped — so a historical replay stays deterministic.
    ///
    /// next's ergonomic twin of legacy `mapper()` (the map half of `graph_node`).
    /// The two graphs run concurrently and touch only at the channel layer, in
    /// lock-step by graph time: each instant, this graph sends the input value and
    /// the worker sends the corresponding result. Like legacy, the sub-graph is
    /// expected to produce a result per input instant (a filtering/delaying
    /// sub-graph desynchronises the lock-step); bound the run by duration (not a
    /// raw cycle count) for exact historical parity — see `docs/port-plan.md`.
    fn spawn_map<O, F>(&self, build: F) -> Stream<Burst<O>>
    where
        T: Clone + Default + Send + 'static,
        O: Clone + Default + Send + 'static,
        F: FnOnce(Stream<Burst<T>>) -> Stream<O> + Send + 'static;

    /// [`spawn_map`](Self::spawn_map) with an optional bound (`None` unbounded,
    /// the default) applied to **both** worker channels — this graph's input to
    /// the worker and the worker's result back — so a slow side back-pressures the
    /// other in realtime. Historical mode is already lock-step, so the bound only
    /// caps in-flight messages there.
    fn spawn_map_bounded<O, F>(&self, buffer: Option<usize>, build: F) -> Stream<Burst<O>>
    where
        T: Clone + Default + Send + 'static,
        O: Clone + Default + Send + 'static,
        F: FnOnce(Stream<Burst<T>>) -> Stream<O> + Send + 'static;
}

impl<T: 'static> StreamOps<T> for Stream<T> {
    __wf_fluent_map!(T);

    __wf_fluent_try_map!(T);

    __wf_fluent_map_filter!(T);

    fn with_time(&self) -> Stream<(NanoTime, T)>
    where
        T: Clone + 'static,
    {
        self.wire(|b, h| b.with_time(h))
    }

    __wf_fluent_ticked_at!(T);

    __wf_fluent_ticked_at_elapsed!(T);

    __wf_fluent_fold!(T);

    __wf_fluent_accumulate!(T);

    __wf_fluent_join!(T);

    __wf_fluent_join_passive!(T);

    __wf_fluent_join3!(T);

    __wf_fluent_try_join!(T);

    __wf_fluent_try_join_passive!(T);

    __wf_fluent_try_join3!(T);

    __wf_fluent_filter!(T);

    __wf_fluent_sample!(T);

    __wf_fluent_merge!(T);

    fn merge_all(&self, others: &[&Stream<T>]) -> Stream<T>
    where
        T: Clone + Default + 'static,
    {
        // No node at all for the degenerate case: `merge_all(&[])` is this
        // stream, exactly as the old empty fold produced.
        if others.is_empty() {
            return self.clone();
        }
        let rest: Vec<Handle<T>> = others.iter().map(|s| s.handle()).collect();
        self.wire(move |b, h| {
            let mut srcs = Vec::with_capacity(rest.len() + 1);
            srcs.push(h);
            srcs.extend(rest);
            b.merge_n(&srcs)
        })
    }

    fn map_n<F>(&self, n: usize, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn(&T) -> T + Clone + 'static,
    {
        let mut s = self.clone();
        for _ in 0..n {
            s = s.map(f.clone());
        }
        s
    }

    fn fan<B, F>(&self, n: usize, branch: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(Stream<T>) -> Stream<B>,
    {
        assert!(n >= 1, "`fan` requires at least one branch");
        // Build every branch first, then fan them in through a single n-ary
        // merge — the branch tails are the merge's upstreams, in branch order,
        // so the tie-break stays "earliest branch that ticked wins".
        let branches: Vec<Stream<B>> = (0..n).map(|_| branch(self.clone())).collect();
        let rest: Vec<&Stream<B>> = branches[1..].iter().collect();
        branches[0].merge_all(&rest)
    }

    __wf_fluent_limit!(T);

    __wf_fluent_throttle!(T);

    __wf_fluent_window!(T);

    __wf_fluent_buffer!(T);

    __wf_fluent_inspect!(T);

    fn logged(&self, label: &str, level: log::Level) -> Stream<T>
    where
        T: Clone + Default + Debug + 'static,
    {
        self.wire(|b, h| b.logged(h, (label.to_string(), level)))
    }

    __wf_fluent_distinct!(T);

    __wf_fluent_drop_small_change!(T);

    __wf_fluent_difference!(T);

    fn not(&self) -> Stream<T>
    where
        T: Clone + Default + Not<Output = T> + 'static,
    {
        self.map(|v| !v.clone())
    }

    __wf_fluent_print!(T);

    __wf_fluent_timed!(T);

    __wf_fluent_delay!(T);

    fn delay_with_reset<U>(&self, delay: Duration, trigger: &Stream<U>) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static,
        U: 'static,
    {
        let trig = trigger.handle();
        self.wire(|b, h| b.delay_with_reset(h, trig, delay))
    }

    fn collapse<OUT>(&self) -> Stream<OUT>
    where
        T: Clone + IntoIterator<Item = OUT> + 'static,
        OUT: Clone + Default + 'static,
    {
        self.map_filter(|x: &T| match x.clone().into_iter().last() {
            Some(last) => (last, true),
            None => (OUT::default(), false),
        })
    }

    __wf_fluent_for_each!(T);

    fn for_each_mut<W, F>(&self, writer: W, f: F) -> Stream<()>
    where
        T: 'static,
        W: 'static,
        F: Fn(&mut W, &T) -> Result<()> + 'static,
    {
        // The one place the RefCell dance lives: a sink hands in an owned `&mut`
        // resource and a plain `Fn(&mut W, &T)`, and this wraps it for `for_each`.
        let writer = RefCell::new(writer);
        self.for_each(move |value: &T| {
            let mut w = writer.borrow_mut();
            f(&mut w, value)
        })
    }

    __wf_fluent_finally!(T);

    fn feedback(&self, sink: &FeedbackSink<T>) -> Stream<T>
    where
        T: Clone + Default + PartialEq + 'static,
    {
        self.wire(|b, h| b.feedback_send(h, sink))
    }

    fn spawn_map<O, F>(&self, build: F) -> Stream<Burst<O>>
    where
        T: Clone + Default + Send + 'static,
        O: Clone + Default + Send + 'static,
        F: FnOnce(Stream<Burst<T>>) -> Stream<O> + Send + 'static,
    {
        self.spawn_map_bounded(None, build)
    }

    fn spawn_map_bounded<O, F>(&self, buffer: Option<usize>, build: F) -> Stream<Burst<O>>
    where
        T: Clone + Default + Send + 'static,
        O: Clone + Default + Send + 'static,
        F: FnOnce(Stream<Burst<T>>) -> Stream<O> + Send + 'static,
    {
        // The worker's input sender, handed back at run start; the send-sink
        // reads it each cycle. Stays on this thread (not `Send`).
        let to_worker: Rc<RefCell<Option<ChannelSender<T>>>> = Rc::new(RefCell::new(None));
        // Send-sink: forward each input value (timestamped, one per instant) to
        // the worker. Being the receiver's trigger, its lower layer runs first
        // each cycle, so the input is sent before the receiver blocks for output.
        let cell = to_worker.clone();
        let sink = self.with_time().for_each(move |tv: &(NanoTime, T)| {
            if let Some(s) = cell.borrow().as_ref() {
                s.send_at(tv.1.clone(), tv.0);
            }
            Ok(())
        });
        let trigger_idx = sink.handle.index();
        // Triggered output receiver + the sender the worker writes results to
        // (the worker→graph direction of the bound).
        let (out_handle, to_main) = self
            .inner
            .borrow_mut()
            .channel_triggered::<O>(trigger_idx, buffer);
        let recv_idx = out_handle.index();
        // Launch the worker at run start (the run params are known only then); it
        // hands its input sender back over a one-shot, which we store for the sink.
        let build = Rc::new(RefCell::new(Some(build)));
        self.inner.borrow_mut().compose_spawn_at_start(
            recv_idx,
            move |run_mode, run_for, _start_time| {
                let build = build
                    .borrow_mut()
                    .take()
                    .expect("invariant: spawn_map worker built once per run (single-run channels)");
                let (back_tx, back_rx) = std::sync::mpsc::channel::<ChannelSender<T>>();
                let to_main = to_main.clone();
                let worker = std::thread::spawn(move || {
                    // `buffer` bounds the graph→worker direction too.
                    run_worker_map(build, to_main, back_tx, buffer, run_mode, run_for);
                });
                let sender_in = back_rx
                    .recv()
                    .map_err(|_| anyhow::anyhow!("spawn_map: worker thread failed to start"))?;
                *to_worker.borrow_mut() = Some(sender_in);
                Ok(StopHandle::new(WorkerGuard {
                    input: to_worker.clone(),
                    worker: Some(worker),
                }))
            },
        );
        Stream {
            inner: self.inner.clone(),
            built: self.built.clone(),
            handle: out_handle,
        }
    }
}

impl Stream<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Stream<u64> {
        self.wire(|b, h| b.count(h))
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

impl<A, B> Stream<(A, B)>
where
    A: Clone + Default + 'static,
    B: Clone + Default + 'static,
{
    /// Decompose a stream of pairs into its two component streams (the legacy
    /// `split`) — sugar over two [`map`](StreamOps::map)s. Both branches tick
    /// whenever the source does.
    pub fn split(&self) -> (Stream<A>, Stream<B>) {
        (self.map(|t| t.0.clone()), self.map(|t| t.1.clone()))
    }
}
