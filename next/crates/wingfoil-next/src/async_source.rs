//! [`produce_async`]: the classic `produce_async` ergonomic — an async closure
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
//! # Two guarantees classic gives that this layer must not drop
//!
//! Classic `produce_async` derives its [`RunParams`] from the graph's own run
//! (in the node's `setup`, from the live `run_mode`/`run_for`/`start_time`) and
//! bounds the producer→graph channel with `buffer_size`. Here the producer task
//! is spawned at *wiring* time, before [`Runner::run`](crate::interp::Runner::run)
//! is called, so the caller has to hand in the [`RunParams`] up front. That
//! makes it possible for the caller to pass params that disagree with the
//! actual `run(run_mode, run_for)` — which classic makes impossible by
//! construction. To close the gap:
//!
//! * **Validation.** In historical mode the run's `start_time` is knowable and
//!   deterministic (it is the `HistoricalFrom(_)` instant). A validating
//!   passthrough on the source checks, on the first burst it sees, that the
//!   caller-supplied `params.start_time` equals the run's real `start_time`
//!   and [aborts the run](anyhow::bail) with context on a mismatch, rather
//!   than silently replaying against a bogus timeline. (`run_mode`/`run_for`
//!   are not observable from an op's [`Ctx`](crate::op::Ctx) at this layer, so
//!   the caller stays responsible for those matching the eventual `run(..)`;
//!   note a `run_mode` mismatch that shifts `start_time` — e.g. declaring
//!   historical while running realtime, or vice versa — is caught here too.)
//! * **Backpressure.** `buffer_size` bounds how far the realtime producer may
//!   run ahead of the graph: the producer takes a permit before each send and
//!   the passthrough returns one per delivered value, so at most ~`buffer_size`
//!   values sit undelivered — the producer blocks instead of growing memory
//!   without limit. See [`produce_async`] for the historical caveat.
//!
//! ```ignore
//! // The graph owns the runtime (lazily created); no `&Handle` to pass. To
//! // embed in your own runtime instead: `GraphBuilder::new().with_async_runtime(rt.handle().clone())`.
//! let g = GraphBuilder::new();
//! let quotes = produce_async_bounded(&g, params, Some(64), |_p| async {
//!     Ok(futures::stream::iter(vec![
//!         Ok((NanoTime::new(100), 1.0)),
//!         Ok((NanoTime::new(200), 2.0)),
//!     ]))
//! })?;
//! ```

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::mpsc;

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use tokio::runtime::{Handle, Runtime};
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::Burst;
use crate::fluent::{GraphBuilder, SourceOps, Stream};
use crate::op::{Activation, Tick};

/// The async runtime a graph's async adapters spawn onto, owned by the graph.
///
/// Legacy wingfoil hides a global `lazy_static` runtime; wingfoil-next used to
/// make every async adapter take a caller-supplied `&Handle`. This is the middle
/// ground (see `docs/runtime-ownership.md`): the `GraphBuilder` owns **one**
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
            .context("creating the graph-owned tokio runtime for wingfoil-next async adapters")?;
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

/// The run parameters handed to a producer closure (mirrors classic
/// `RunParams`), so a producer can choose a historical vs live data source.
///
/// These describe the run the graph will actually be driven with. They must
/// match the `run(run_mode, run_for)` the caller ultimately invokes; a
/// disagreement in `start_time` is rejected at run time (see the module docs).
/// For the run *bound*, prefer emitting a finite stream and letting the
/// receiver stop at end-of-stream.
#[derive(Clone, Copy, Debug)]
pub struct RunParams {
    pub run_mode: RunMode,
    pub run_for: RunFor,
    pub start_time: NanoTime,
}

/// Drive a graph source from an async producer of timestamped values. See the
/// module docs. Returns the source [`Stream<Burst<T>>`].
///
/// The producer closure receives [`RunParams`]; it must return a stream of
/// `Result<(NanoTime, T)>`. Each `Ok((t, v))` is delivered at graph time `t`
/// (historical replay) or live (realtime); an `Err` aborts the run.
///
/// `params` must describe the same run passed to
/// [`run`](crate::interp::Runner::run): in historical mode the run's
/// `start_time` is validated against `params.start_time` and a mismatch aborts
/// the run with context (the caller-supplied params are never trusted blind).
///
/// This is the unbounded variant — a fast producer feeding a slower graph can
/// accumulate an arbitrarily large backlog. Use [`produce_async_bounded`] to
/// apply `buffer_size` back-pressure in realtime.
pub fn produce_async<T, F, Fut, S>(
    g: &GraphBuilder,
    params: RunParams,
    run: F,
) -> anyhow::Result<Stream<Burst<T>>>
where
    T: Clone + Default + Send + 'static,
    F: FnOnce(RunParams) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<S>> + Send + 'static,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
{
    produce_async_bounded(g, params, None, run)
}

/// [`produce_async`] with `buffer_size` back-pressure (mirrors classic
/// `produce_async`'s `buffer_size`). See the module docs.
///
/// `params` must describe the same run passed to
/// [`run`](crate::interp::Runner::run): in historical mode the run's
/// `start_time` is validated against `params.start_time` and a mismatch aborts
/// the run with context (the caller-supplied params are never trusted blind).
///
/// `buffer_size` bounds the producer→graph backlog in **realtime**: `Some(n)`
/// lets the producer run at most ~`n` values ahead of the graph before it
/// blocks (back-pressure); `None` is unbounded (a fast producer feeding a
/// slower graph can accumulate an arbitrarily large backlog). `Some(0)` is
/// treated as `Some(1)`. Historical replay collects the whole timestamped
/// stream up front (the producer sends everything, then closes), so
/// `buffer_size` is not applied there — bounding it would deadlock that
/// up-front collection.
pub fn produce_async_bounded<T, F, Fut, S>(
    g: &GraphBuilder,
    params: RunParams,
    buffer_size: Option<usize>,
    run: F,
) -> anyhow::Result<Stream<Burst<T>>>
where
    T: Clone + Default + Send + 'static,
    F: FnOnce(RunParams) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<S>> + Send + 'static,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
{
    // The graph owns the runtime (created lazily here on first async use, or a
    // caller override); the producer task spawns onto it at wiring time.
    let handle = g.async_runtime_handle()?;
    let (stream, sender) = g.channel::<T>();

    // Realtime backpressure: a bounded permit channel. The producer takes a
    // permit before each send; the validating passthrough returns one per
    // delivered value. Only wired in realtime — a historical run collects the
    // whole stream at graph start, so throttling the producer there would
    // deadlock that collection. Dropping the receiver (when the graph is torn
    // down) closes the channel, which unblocks a parked producer.
    let (mut permit_tx, mut permit_rx) = match (params.run_mode, buffer_size) {
        (RunMode::RealTime, Some(n)) => {
            // futures' bounded channel grants each sender one extra slot on top
            // of `buffer`, so `n - 1` bounds in-flight values to ~`n`.
            let (tx, rx) = futures::channel::mpsc::channel::<()>(n.max(1) - 1);
            (Some(tx), Some(rx))
        }
        _ => (None, None),
    };

    // Validating passthrough: forwards each burst unchanged, checks the
    // caller's declared params against the real run once, and releases permits.
    let mut checked = false;
    let declared = params;
    let validated = stream.wire(move |b, h| {
        b.register_op1(
            h,
            "produce_async::validate",
            Activation::NONE,
            (),
            || (),
            move |_cfg: &mut (), _state: &mut (), input: &Burst<T>, ctx| {
                if !checked {
                    checked = true;
                    if let RunMode::HistoricalFrom(_) = declared.run_mode {
                        let actual = ctx.start_time();
                        if declared.start_time != actual {
                            anyhow::bail!(
                                "produce_async: caller-supplied RunParams.start_time ({:?}) \
                                 does not match the graph run's start_time ({:?}); RunParams \
                                 must describe the same run passed to run(run_mode, run_for)",
                                declared.start_time,
                                actual,
                            );
                        }
                    }
                }
                // Return one permit per delivered value so the producer may
                // advance. Draining exactly `len` keeps the bound intact
                // (never freeing capacity for values not yet delivered).
                if let Some(rx) = permit_rx.as_mut() {
                    for _ in 0..input.len() {
                        if rx.try_recv().is_err() {
                            break;
                        }
                    }
                }
                Ok(Tick::Value(input.clone()))
            },
        )
    });

    handle.spawn(async move {
        match run(params).await {
            Err(e) => {
                let _ = sender.send_error(e);
            }
            Ok(source) => {
                futures::pin_mut!(source);
                while let Some(item) = source.next().await {
                    match item {
                        Ok((t, v)) => {
                            // Take a permit first (realtime only). A closed
                            // permit channel means the graph is gone — a normal
                            // teardown race; stop producing.
                            if let Some(tx) = permit_tx.as_mut()
                                && tx.send(()).await.is_err()
                            {
                                return;
                            }
                            // `send_at` returns false once the receiver is gone —
                            // a normal teardown race; stop producing.
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
    Ok(validated)
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
///   [`produce_async_bounded`]'s producer blocks on a full permit channel) until
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
/// under `RunFor::Cycles(1)` — matching classic's `teardown()`-time surfacing
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
/// This is the concurrent-within-burst shape kafka's producer wants (classic
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
