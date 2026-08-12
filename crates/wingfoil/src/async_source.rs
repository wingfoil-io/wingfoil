//! [`produce_async`]: the legacy `produce_async` ergonomic — an async closure
//! that yields *timestamped* values driving a graph source — over the
//! [`channel`](crate::fluent::SourceOps::channel) layer.
//!
//! The closure returns a [`futures::Stream`] of `Result<(NanoTime, T)>`. A
//! task spawned on the graph's tokio runtime drives it, forwarding each
//! value to the channel (timestamped, so it works in **both** run modes —
//! deterministic historical replay on the graph clock, or live realtime) and
//! closing at end-of-stream. A producer error propagates into the graph and
//! aborts the run. The graph source emits [`Burst<T>`](crate::Burst),
//! never latest-wins.
//!
//! Its sink counterpart is [`consume_async`]: an async consumer that drains each
//! [`Burst<T>`](crate::Burst) to a background task over a bounded channel
//! (back-pressure + preserved write order), so networked sinks run off the
//! single-threaded engine via [`for_each`](crate::fluent::StreamOps::for_each)
//! instead of blocking a `cycle` on I/O.
//!
//! Gated behind the `async` feature (it pulls in `tokio` + `futures`); the
//! core engine stays executor-free.
//!
//! # Two guarantees legacy gives that this layer must not drop
//!
//! Legacy `produce_async` derives its [`RunParams`] from the graph's own run
//! (in the node's `setup`, from the live `run_mode`/`run_for`/`start_time`) and
//! bounds the producer→graph channel with `buffer_size`. Wingfoil matches both:
//!
//! * **Params from the run.** The producer task spawns in `start()` (deferred via
//!   `source_at_start` — nothing runs at wiring), and its [`RunParams`] are
//!   derived from the *actual* run at that point (the deferred `setup` is handed
//!   the live `run_mode`/`run_for`/`start_time`). So — like legacy — there is no
//!   caller-declared params to disagree with the run; the earlier declare-up-front
//!   footgun (and its validating passthrough) is gone.
//! * **Backpressure.** `buffer_size` bounds how far the producer may run ahead
//!   of the graph, in **both** run modes (matching legacy's bounded
//!   `channel_pair`): the producer takes a permit before each send and the
//!   passthrough returns one per delivered value, so at most ~`buffer_size`
//!   values sit undelivered — the producer waits instead of growing memory
//!   without limit. In historical this bounds a time-sliced replay to a lazy,
//!   pipelined per-slice fetch (the receiver drains incrementally via
//!   `pump_historical`, freeing permits as the graph clock advances); in realtime
//!   it caps a fast subscriber's backlog. `None` is unbounded in both.
//!
//! ```ignore
//! // The graph owns the runtime (lazily created); no `&Handle` to pass. To
//! // embed in your own runtime instead: `GraphBuilder::new().with_async_runtime(rt.handle().clone())`.
//! let g = GraphBuilder::new();
//! let quotes = produce_async(&g, |_p| async {
//!     Ok(futures::stream::iter(vec![
//!         Ok((NanoTime::new(100), 1.0)),
//!         Ok((NanoTime::new(200), 2.0)),
//!     ]))
//! }, Some(64))?;
//! ```

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::mpsc;

use anyhow::Context;
use futures::StreamExt;
use tokio::runtime::{Handle, Runtime};
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::Burst;
use crate::fluent::{GraphBuilder, Stream};
use crate::interp::StopHandle;
use crate::op::{Activation, Tick};

/// The async runtime a graph's async adapters spawn onto, owned by the graph.
///
/// Legacy wingfoil hides a global `lazy_static` runtime; wingfoil used to
/// make every async adapter take a caller-supplied `&Handle`. This is the middle
/// ground (see `docs/decisions/runtime-ownership.md`): the `GraphBuilder` owns **one**
/// runtime, created lazily the first time an async adapter asks for a handle and
/// dropped at teardown — so all async adapters in a graph share it, the common
/// call needs no `&Handle`, and there is no never-dropped global. A caller can
/// still inject their own runtime via
/// [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime)
/// (the override) to embed the graph in an existing async application.
///
/// Held in the executor-free core only as an opaque
/// [`AsyncRuntimeSlot`](crate::interp::AsyncRuntimeSlot); all tokio types stay
/// behind the `async` feature here.
#[derive(Default)]
pub struct GraphRuntime {
    /// Caller override; when set, adapters use this handle and the graph owns
    /// (and drops) nothing.
    override_handle: Option<Handle>,
    /// The graph's own runtime, created lazily on first use when no override is
    /// set. Dropped when this slot is dropped — i.e. at [`Runner`] teardown,
    /// after every node — which stops any still-running producer tasks.
    ///
    /// [`Runtime`]: tokio::runtime::Runtime
    /// [`Runner`]: crate::interp::Runner
    owned: Option<Runtime>,
    /// Cached handle to `owned`, so repeated `handle()` calls hand every adapter
    /// the *same* runtime rather than standing up a new one each time.
    cached: Option<Handle>,
}

impl GraphRuntime {
    /// Resolve the handle to spawn async work onto: the caller override if set,
    /// otherwise the graph's own runtime (created lazily and cached here on first
    /// use). Fallible only on the first, owned-runtime creation.
    pub fn handle(&mut self) -> anyhow::Result<Handle> {
        if let Some(handle) = &self.override_handle {
            return Ok(handle.clone());
        }
        if let Some(handle) = &self.cached {
            return Ok(handle.clone());
        }
        let runtime = Runtime::new()
            .context("creating the graph-owned tokio runtime for wingfoil async adapters")?;
        let handle = runtime.handle().clone();
        self.owned = Some(runtime);
        self.cached = Some(handle.clone());
        Ok(handle)
    }

    /// Install a caller-supplied runtime handle as the override. Adapters wired
    /// afterwards spawn onto it, and the graph creates/owns no runtime of its
    /// own.
    pub fn set_override(&mut self, handle: Handle) {
        self.override_handle = Some(handle);
    }
}

/// The run parameters handed to a producer closure (mirrors legacy
/// `RunParams`), so a producer can choose a historical vs live data source.
///
/// These describe the run the graph is actually being driven with — a
/// [`produce_async`] producer receives them derived from the live run at graph
/// start, so they always match `run(run_mode, run_for)` (no caller declaration to
/// disagree). For the run *bound*, prefer emitting a finite stream and letting the
/// receiver stop at end-of-stream.
#[derive(Clone, Copy, Debug)]
pub struct RunParams {
    pub run_mode: RunMode,
    pub run_for: RunFor,
    pub start_time: NanoTime,
}

/// Drive a graph source from an async producer of timestamped values, matching
/// legacy `produce_async`'s `(closure, buffer_size)` signature. See the module
/// docs. Returns the source [`Stream<Burst<T>>`].
///
/// The producer closure receives the run's [`RunParams`] — derived from the
/// actual [`run`](crate::interp::Runner::run) at graph start, not declared up
/// front — and must return a stream of `Result<(NanoTime, T)>`. Each `Ok((t, v))`
/// is delivered at graph time `t` (historical replay) or live (realtime); an
/// `Err` aborts the run.
///
/// `buffer_size` bounds the producer→graph backlog in **both** run modes
/// (matching legacy's bounded `channel_pair`); `None` is unbounded (a fast
/// producer feeding a slower graph can accumulate an arbitrarily large backlog).
/// What `Some(n)` counts differs by mode, mirroring how each groups values:
///
/// * **Realtime** — ~`n` *values* ahead of the graph's delivery point (values
///   coalesce into a burst by arrival, so the bound is per value).
/// * **Historical** — ~`n` *timestamp-groups* ahead (same-time values ride one
///   atomic burst, exactly as legacy, so an arbitrarily large same-time burst
///   is never split and never counts as more than one slot). This is what makes
///   a lazy time-sliced source stay bounded and pipelined: the receiver drains
///   incrementally (`pump_historical`) as the graph clock advances, freeing a
///   permit per delivered group, so the producer only fetches the next slice's
///   rows once there is room — never materialising the whole replay. (Safe now
///   that the receiver drains incrementally; the old block-collect receiver
///   *would* have deadlocked under a bound, which is why historical throttling
///   used to be skipped.)
///
/// **Look-ahead floor.** The self-driven historical receiver reads one group
/// *past* the current instant to close a same-time group before delivering it,
/// so it must hold ~2 groups before the first delivery frees a permit. A bound
/// of `Some(1)` would wait on a permit that only the delivery it is blocking can
/// release — a deadlock — so the effective bound is floored to 2 (`Some(0)`/
/// `Some(1)` behave as `Some(2)`). Values and tick times are unchanged by the
/// bound; only the producer's pace is.
pub fn produce_async<T, F, Fut, S>(
    g: &GraphBuilder,
    run: F,
    buffer_size: Option<usize>,
) -> anyhow::Result<Stream<Burst<T>>>
where
    T: Clone + Default + Send + 'static,
    F: FnOnce(RunParams) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<S>> + Send + 'static,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
{
    // The graph owns the runtime (created lazily here on first async use, or a
    // caller override). The producer task is spawned in `start()` — not at wiring
    // — via `source_at_start`, so an adapter's I/O (connect / subscribe) is
    // established at run start, matching legacy and keeping wiring side-effect
    // free (nothing runs until `run()`). Re-run is still a follow-on: the source
    // inherits `channel`'s single-run restriction. See
    // `docs/decisions/source-lifecycle-defer-to-start.md`.
    let handle = g.async_runtime_handle()?;

    // Backpressure: a permit semaphore, created when a bound is requested. Active
    // in **both** run modes (matching legacy's bounded `channel_pair`): the
    // producer acquires (and forgets) one permit before each unit and the
    // passthrough adds one back per delivered unit, so the producer runs at most
    // ~`buffer_size` units ahead of the graph's delivery point. In historical this
    // keeps a lazy time-sliced source bounded and pipelined — safe now that the
    // receiver drains incrementally (`pump_historical`); the old block-collect
    // receiver *would* have deadlocked under a bound, which is why this used to be
    // realtime-only.
    //
    // The budget is floored to **2**: the self-driven historical receiver reads
    // one group *past* `now` to close a same-time group before delivering it, so
    // the producer must send the current group *and* the next before the first
    // delivery adds a permit back. A budget of 1 deadlocks — the receiver blocks
    // in `recv()` for that next group while the producer waits on the permit only
    // that blocked delivery would add. A semaphore makes the budget exact (a
    // bounded channel's buffer-vs-usable-in-flight count is ambiguous).
    let permit_sem =
        buffer_size.map(|n| std::sync::Arc::new(tokio::sync::Semaphore::new(n.max(2))));

    // The producer is established at `start()`: `run`/`permit_tx` are once-only
    // (taken on the first — and, single-run, only — start), and the returned
    // `StopHandle` aborts the task at teardown (a finished historical producer is
    // already gone; a live realtime producer is stopped here). The producer's
    // `RunParams` are derived from the *actual* run here — there is no
    // caller-declared params to disagree with, so no validation is needed.
    let mut run = Some(run);
    let producer_sem = permit_sem.clone();
    let stream_handle = g.with_builder(move |b| {
        b.source_at_start_with_params::<T, _>(None, move |sender, run_mode, run_for, start_time| {
            let run = run
                .take()
                .expect("invariant: produce_async producer is single-run");
            let params = RunParams {
                run_mode,
                run_for,
                start_time,
            };
            // Permits bound the producer in both run modes (see above).
            let sem = producer_sem.clone();
            // Historical bounds by **group** (distinct timestamp), realtime by
            // value. In historical the receiver reads one value *past* a group to
            // close it before delivering, so a same-time burst must be sent whole
            // before the producer waits — otherwise a burst larger than the bound
            // would deadlock on that read. Taking one permit per group (not per
            // value) sends the whole burst under one permit; the passthrough
            // returns one permit per delivered group. Realtime coalesces by
            // arrival, not timestamp, so it stays per-value (take one per value,
            // return `len` per burst) — its receiver never blocks to close a group.
            let is_realtime = matches!(run_mode, RunMode::RealTime);
            let task = handle.spawn(async move {
                match run(params).await {
                    Err(e) => {
                        let _ = sender.send_error(e);
                    }
                    Ok(source) => {
                        futures::pin_mut!(source);
                        let mut last_time: Option<NanoTime> = None;
                        while let Some(item) = source.next().await {
                            match item {
                                Ok((t, v)) => {
                                    // Acquire + forget one permit per value
                                    // (realtime) or per new group (historical); the
                                    // passthrough adds it back on delivery. This
                                    // await paces the producer to the graph.
                                    let new_group = last_time != Some(t);
                                    last_time = Some(t);
                                    if (is_realtime || new_group)
                                        && let Some(s) = sem.as_ref()
                                    {
                                        match s.acquire().await {
                                            Ok(p) => p.forget(),
                                            // Semaphore closed — graph gone
                                            // (teardown race); stop.
                                            Err(_) => return,
                                        }
                                    }
                                    // `send_at` returns false once the receiver
                                    // is gone — a teardown race; stop.
                                    if !sender.send_at(v, t) {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let _ = sender.send_error(e);
                                    return;
                                }
                            }
                        }
                        let _ = sender.close();
                    }
                }
            });
            Ok(StopHandle::new(AbortOnDrop(task)))
        })
    });
    let stream = g.wrap(stream_handle);

    // Backpressure passthrough: adds permits back as values are delivered so the
    // producer may advance. The count mirrors how the producer *took* them (see
    // the producer loop): realtime took one per value → add `len`; historical took
    // one per group and delivers exactly one group per burst → add 1.
    let validated = stream.wire(move |b, h| {
        let permit_sem = permit_sem.clone();
        b.register_op1(
            h,
            "produce_async::backpressure",
            Activation::NONE,
            (),
            || (),
            move |_cfg: &mut (), _state: &mut (), input: &Burst<T>, ctx| {
                if let Some(sem) = permit_sem.as_ref() {
                    let n = if matches!(ctx.run_mode(), RunMode::RealTime) {
                        input.len()
                    } else {
                        1
                    };
                    sem.add_permits(n);
                }
                Ok(Tick::Value(input.clone()))
            },
        )
    });
    Ok(validated)
}

/// Aborts a deferred [`produce_async`] producer task when the run tears down (the
/// [`StopHandle`] held by `source_at_start` drops). A finished historical producer
/// is already gone; a live realtime producer (a subscriber socket) is stopped
/// here — the deferred-source analogue of the wiring-spawn model's
/// receiver-dropped stop.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---------------------------------------------------------------------------
// consume_async — the sink counterpart of produce_async
// ---------------------------------------------------------------------------

/// Drive a graph **sink** from an async consumer, so networked writes run off
/// the single-threaded engine instead of blocking a `cycle` on I/O. The mirror
/// image of [`produce_async`]: where the source hands values *from* a background
/// task *into* the graph, this hands each burst's values *out of* the graph to a
/// background task.
///
/// Returns **two** closures: a `sink` to plug into
/// [`for_each`](crate::fluent::StreamOps::for_each) (one send per burst), and a
/// `flush` to wire as the sink's [`finally`](crate::fluent::StreamOps::finally)
/// teardown. The `flush` is what surfaces a final-cycle write error (see the
/// teardown section) — **always wire it**, or the last cycle's write error is
/// lost:
///
/// ```ignore
/// // The graph owns the runtime (lazily created); pass `&g`, not a `&Handle`.
/// let g = GraphBuilder::new();
/// let (sink, flush) = consume_async(&g, Some(64), |v| async move {
///     write_somewhere(v).await // returns anyhow::Result<()>
/// })?;
/// let node = some_burst_stream.for_each(sink).finally(flush);
/// ```
///
/// # Guarantees
///
/// * **Order is preserved.** Every value is drained by a **single** consumer
///   task that awaits each `run(value)` to completion before taking the next, so
///   writes land in the exact order the graph produced them (values within a
///   burst, and across bursts).
/// * **Back-pressure.** `buffer_size` bounds how far the graph may run ahead of a
///   slower sink: `Some(n)` uses a bounded channel of ~`n` and the sink closure
///   *blocks the graph thread* on a full channel (on `handle`, exactly as
///   [`produce_async`]'s producer blocks on a full permit channel) until
///   the consumer drains one — so at most ~`n` values sit unwritten, memory does
///   not grow without bound, and nothing is dropped. `None` is unbounded (a fast
///   graph feeding a slow sink accumulates an arbitrarily large backlog). Unlike
///   a source, back-pressure applies in **both** run modes here — the sink never
///   collects up front, so bounding it can never deadlock.
/// * **Errors propagate into the graph.** A write error is reported over an error
///   channel (not a lock — the graph execution path stays lock-free) and the
///   consumer task stops. The sink closure polls that channel on entry to each
///   cycle and, once an error is present, [aborts the run](anyhow::bail) with
///   context — mirroring how [`produce_async`] surfaces a producer error on the
///   next cycle rather than mid-await. A closed data channel (the consumer having
///   stopped after an error) is likewise turned into an aborting error on the
///   next send. The **final** cycle's write error — which has no later cycle to
///   surface it — is caught by the `flush` teardown (below).
///
/// # Teardown — the `flush` closure surfaces the final write error
///
/// The `flush` closure drops the sender (so the consumer's `recv` ends), **blocks
/// until the consumer has drained every queued write**, then — unlike a `Drop`,
/// which cannot return a `Result` — checks the error channel one last time and
/// [aborts the run](anyhow::bail) if that final drain failed. Wired as the sink's
/// [`finally`](crate::fluent::StreamOps::finally), it runs on the graph thread
/// after the last cycle (even after a cycle error), and the engine folds its
/// error into the run result. This is what lets a sink abort the run
/// deterministically on the **last** write — e.g. etcd's `force:false` conditional
/// under `RunFor::Cycles(1)` — matching legacy's `teardown()`-time surfacing
/// (`AsyncConsumerNode::teardown` does `block_on(handle)??`).
///
/// If `flush` is never wired, a [`Drop`] safety-net still drains every queued
/// write at graph teardown (so nothing is lost), but — being a `Drop` — cannot
/// surface that final error. Always wire `flush`.
///
/// # Runtime requirement (the same `block_on` footgun as the etcd sink)
///
/// The sink closure and the `flush` teardown drive the runtime with
/// [`Handle::block_on`] on the graph thread, so **the graph must be built, run,
/// and dropped from a non-async thread** (`main`, a `#[test]` fn). Driving it
/// from inside an async context makes those `block_on` calls panic.
///
/// Gated behind the `async` feature, like [`produce_async`].
#[allow(clippy::type_complexity)]
pub fn consume_async<T, F, Fut>(
    g: &GraphBuilder,
    buffer_size: Option<usize>,
    run: F,
) -> anyhow::Result<(
    Box<dyn Fn(&Burst<T>) -> anyhow::Result<()>>,
    Box<dyn Fn(&()) -> anyhow::Result<()>>,
)>
where
    T: Clone + Default + Send + 'static,
    F: FnMut(T) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    // Payload = one value. The sink feeds the burst's values into the channel
    // one at a time; the consumer awaits each `run(value)` before the next.
    let (send_one, flush) = spawn_sink::<T, F, Fut>(g, buffer_size, run)?;
    let sink = move |burst: &Burst<T>| -> anyhow::Result<()> {
        for item in burst.iter() {
            send_one(item.clone())?;
        }
        Ok(())
    };
    Ok((Box::new(sink), flush))
}

/// [`consume_async`], but the consumer processes a **whole burst at a time**
/// instead of one value at a time — so a sink can act on the burst's values
/// *concurrently* (e.g. hand them all to a producer and drain their delivery
/// futures together) while the single consumer still preserves order *across*
/// bursts.
///
/// The `run` closure receives one `Vec<T>` per burst (empty bursts are skipped,
/// never handed to `run`). Everything else matches [`consume_async`]: the
/// returned `(sink, flush)` pair wires the same way (`for_each(sink).finally(flush)`),
/// back-pressure/ordering/error-surfacing are identical (here `buffer_size`
/// bounds bursts, not values), and the final burst's error surfaces via `flush`.
///
/// This is the concurrent-within-burst shape kafka's producer wants (legacy
/// `kafka_pub` drained a burst's sends together via `FuturesUnordered`, one
/// broker roundtrip per burst); the per-value [`consume_async`] would serialise
/// them into N roundtrips.
#[allow(clippy::type_complexity)]
pub fn consume_async_bursts<T, F, Fut>(
    g: &GraphBuilder,
    buffer_size: Option<usize>,
    run: F,
) -> anyhow::Result<(
    Box<dyn Fn(&Burst<T>) -> anyhow::Result<()>>,
    Box<dyn Fn(&()) -> anyhow::Result<()>>,
)>
where
    T: Clone + Default + Send + 'static,
    F: FnMut(Vec<T>) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    // Payload = one burst (as an owned `Vec<T>`). The whole burst is handed to
    // `run` at once so it can process the values concurrently.
    let (send_one, flush) = spawn_sink::<Vec<T>, F, Fut>(g, buffer_size, run)?;
    let sink = move |burst: &Burst<T>| -> anyhow::Result<()> {
        if !burst.is_empty() {
            send_one(burst.iter().cloned().collect::<Vec<T>>())?;
        }
        Ok(())
    };
    Ok((Box::new(sink), flush))
}

/// The shared machinery behind [`consume_async`] and [`consume_async_bursts`]:
/// spawn the single consumer task over a `buffer_size`-bounded channel, and hand
/// back a `send_one` closure (feed one payload `P` to the consumer, surfacing a
/// prior async error) plus the `flush` teardown. The two public entry points
/// differ only in the payload type — one value vs one burst — and in how their
/// sink closure chunks a burst into `send_one` calls.
#[allow(clippy::type_complexity)]
fn spawn_sink<P, F, Fut>(
    g: &GraphBuilder,
    buffer_size: Option<usize>,
    run: F,
) -> anyhow::Result<(
    Box<dyn Fn(P) -> anyhow::Result<()>>,
    Box<dyn Fn(&()) -> anyhow::Result<()>>,
)>
where
    P: Send + 'static,
    F: FnMut(P) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    // The graph owns the runtime (created lazily here on first async use, or a
    // caller override); the consumer task spawns onto it, and the sink closure /
    // flush teardown drive it with `block_on` on the graph thread.
    let handle = g.async_runtime_handle()?;

    // Error channel: the consumer task reports the first write error here; the
    // sink closure polls it (non-blocking) each cycle and the flush teardown
    // polls it once more after the final drain. A channel — not a lock — keeps
    // the graph execution path lock-free.
    let (err_tx, err_rx) = mpsc::channel::<String>();

    // Bounded (back-pressure) or unbounded data channel, per `buffer_size`.
    let (tx, mut rx) = sink_channel::<P>(buffer_size);

    let mut run = run;
    let task = handle.spawn(async move {
        // Single consumer: await each payload to completion before the next, so
        // writes land in the order the graph produced them.
        while let Some(item) = rx.recv().await {
            if let Err(e) = run(item).await {
                // Report the error, then return — dropping the receiver closes
                // the data channel so the sink's next send fails fast too.
                let _ = err_tx.send(format!("{e:#}"));
                return;
            }
        }
    });

    // Shared between the sink and flush closures — both run on the graph thread,
    // never concurrently (cycles finish before teardown), so an `Rc<RefCell<_>>`
    // is sufficient and no lock touches the execution path.
    let shared = Rc::new(RefCell::new(SinkShared {
        handle: handle.clone(),
        tx: Some(tx),
        task: Some(task),
        err_rx,
        flushed: false,
    }));

    let send_handle = handle.clone();
    let send_shared = shared.clone();
    let send_one = move |item: P| -> anyhow::Result<()> {
        let s = send_shared.borrow();
        // Surface a prior async write error before doing more work.
        if let Ok(msg) = s.err_rx.try_recv() {
            anyhow::bail!("consume_async: background sink write failed: {msg}");
        }
        let tx =
            s.tx.as_ref()
                .expect("invariant: sink sender present during the run");
        if tx.send_blocking(&send_handle, item).is_err() {
            // A closed data channel means the consumer task stopped, which
            // only happens after a write error; surface it with context.
            return match s.err_rx.try_recv() {
                Ok(msg) => Err(anyhow::anyhow!(
                    "consume_async: background sink write failed: {msg}"
                )),
                Err(_) => Err(anyhow::anyhow!(
                    "consume_async: background sink task ended unexpectedly"
                )),
            };
        }
        Ok(())
    };

    let flush = move |_: &()| -> anyhow::Result<()> { shared.borrow_mut().flush() };

    Ok((Box::new(send_one), Box::new(flush)))
}

/// Shared teardown state for a [`consume_async`] sink: the sender to close, the
/// consumer task to join, and the error channel to poll for the final write
/// error. Held behind an `Rc<RefCell<_>>` by both the sink and flush closures.
struct SinkShared<T> {
    handle: Handle,
    tx: Option<SinkTx<T>>,
    task: Option<tokio::task::JoinHandle<()>>,
    err_rx: mpsc::Receiver<String>,
    flushed: bool,
}

impl<T> SinkShared<T> {
    /// Close the sender, block until the consumer has drained every queued
    /// write, then surface a final write error if the last drain failed.
    /// Idempotent — the `flush`/`Drop` pair may both fire.
    fn flush(&mut self) -> anyhow::Result<()> {
        if self.flushed {
            return Ok(());
        }
        self.flushed = true;
        // Drop the sender so the consumer task's `recv()` returns `None` and it
        // finishes draining every queued write.
        self.tx.take();
        if let Some(task) = self.task.take() {
            // Runs on the (non-async) graph thread at teardown, so `block_on`
            // is safe here. The joined task never touches `self`.
            let _ = self.handle.block_on(task);
        }
        // Unlike a `Drop`, teardown can turn an already-`Ok` run into `Err`:
        // surface a final-cycle write error the sink closure never got to see.
        if let Ok(msg) = self.err_rx.try_recv() {
            anyhow::bail!("consume_async: background sink write failed: {msg}");
        }
        Ok(())
    }
}

impl<T> Drop for SinkShared<T> {
    fn drop(&mut self) {
        // Safety-net flush for a sink whose `flush` teardown was never wired:
        // still drain every queued write at graph teardown so nothing is lost.
        // A `Drop` cannot surface the final error (no `Result`) — that is what
        // the `flush` teardown is for.
        let _ = self.flush();
    }
}

/// Sender half of the sink's data channel — bounded (back-pressured) or
/// unbounded, chosen by `buffer_size`.
enum SinkTx<T> {
    Bounded(tokio::sync::mpsc::Sender<T>),
    Unbounded(tokio::sync::mpsc::UnboundedSender<T>),
}

impl<T: Send + 'static> SinkTx<T> {
    /// Hand a value to the consumer task. Bounded: block the caller (on
    /// `handle`) until the channel has capacity (back-pressure). Unbounded:
    /// never blocks. `Err(())` means the channel is closed (consumer stopped).
    fn send_blocking(&self, handle: &Handle, item: T) -> Result<(), ()> {
        match self {
            SinkTx::Bounded(s) => handle.block_on(s.send(item)).map_err(|_| ()),
            SinkTx::Unbounded(s) => s.send(item).map_err(|_| ()),
        }
    }
}

/// Receiver half of the sink's data channel.
enum SinkRx<T> {
    Bounded(tokio::sync::mpsc::Receiver<T>),
    Unbounded(tokio::sync::mpsc::UnboundedReceiver<T>),
}

impl<T> SinkRx<T> {
    async fn recv(&mut self) -> Option<T> {
        match self {
            SinkRx::Bounded(r) => r.recv().await,
            SinkRx::Unbounded(r) => r.recv().await,
        }
    }
}

fn sink_channel<T>(buffer_size: Option<usize>) -> (SinkTx<T>, SinkRx<T>) {
    match buffer_size {
        // `Some(0)` would be a zero-capacity channel (a panic); treat it as 1.
        Some(n) => {
            let (tx, rx) = tokio::sync::mpsc::channel::<T>(n.max(1));
            (SinkTx::Bounded(tx), SinkRx::Bounded(rx))
        }
        None => {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<T>();
            (SinkTx::Unbounded(tx), SinkRx::Unbounded(rx))
        }
    }
}
