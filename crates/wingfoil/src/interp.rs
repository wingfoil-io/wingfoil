//! The interpreted engine: dynamic wiring and execution of [`Op`]s.
//!
//! The engine owns everything the op does not: the value slot for each
//! node's output, the op's `Cfg` + `State`, the edges, and the dispatch
//! loop (driven by the shared [`Kernel`]). Each node crosses exactly one
//! dyn boundary — a closure adapting the monomorphic [`Op::cycle`] to a
//! uniform signature. Inside that closure the op code is the *same
//! monomorphized function* a compiled runner calls; the engines share
//! semantics by construction.
//!
//! Execution model: a sparse dirty-list, matching legacy wingfoil's
//! `dirty_nodes_by_layer` (see `docs/planning/port-plan.md` "Phase 4.5"). At `build()`
//! each node gets an *active-downstream* adjacency list. Each cycle seeds a
//! work set from the frontier — `always` busy-poll ops and kernel-marked
//! callback-activated ops (tickers, `delay` pops, feedback source, channel
//! replay) — then propagates the tick frontier forward: a node that ticks
//! marks its active downstream neighbours dirty. The work set drains in
//! ascending node **index** order (an index min-heap); wiring order is a valid
//! topological order over *all* edges — active and passive, since the fluent
//! API forces a stream to exist before it is referenced — so each node fires
//! exactly once after everything it reads. This is glitch-free, gives results
//! **identical** to legacy wingfoil (and byte-identical to the previous
//! full-index sweep it replaces), but per-cycle work is proportional to the
//! nodes that actually fire, not the graph size `N`.
//!
//! Value slots are reached only through [`SlotRef<T>`] — the frozen access
//! boundary between ops and the value store. Each `SlotRef` wraps an individual
//! `Rc<RefCell<T>>` today, but ops only `borrow()`/`borrow_mut()` it, never the
//! concrete cell, so the store can move to a contiguous arena/SoA later as an
//! internal swap without touching a single capture site (the Phase 4.5
//! "freeze the slot boundary" mitigation, made *by type* not just convention;
//! see the port plan). `run` is fallible — it returns the first
//! `start`/`cycle`/`stop`/`teardown` error (with node context) and still runs
//! cleanup afterwards, matching the legacy engine.

use std::any::Any;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, bail};

/// Process-unique id stamped on every [`Builder`] (and its [`Handle`]s) so a
/// handle used with a *different* builder's [`Runner`] is caught by a
/// `debug_assert` rather than silently returning the wrong node's value.
static NEXT_BUILDER_ID: AtomicU64 = AtomicU64::new(0);

use crate::Burst;
use crate::channel::{ChannelSender, Message, Tx};
use crate::introspect::{GraphSnapshot, NodeInfo};
use crate::latency::{
    HasLatency, LatencyReport, LatencyReportCfg, LatencyReportEach, LatencyStats,
};
use crate::op::{Activation, CompositePhase, Ctx, Op, Tick};
use crate::ops::{CombineN, Join, Join3, MergeN, Never, Poll, TryJoin, TryJoin3, WithTime};
use crate::pool::{PoolLoan, Pooled, PooledSender};
use wingfoil::{Kernel, KernelWaker, ReadyReceiver, TimerPolicy, waker_channel};
use wingfoil::{NanoTime, RunFor, RunMode, TimeQueue};

// ---------------------------------------------------------------------------
// Engine span instrumentation (the `instrument-*` features)
//
// Ported from legacy wingfoil's `graph.rs`, feature for feature. Two shapes
// are in play: the whole-function spans (`run`, `run_dynamic`, `build`) use
// `#[cfg_attr(feature = "…", tracing::instrument(skip_all))]` directly at the
// definition, exactly as legacy does; the spans that must scope to a *part* of
// a function — a lifecycle loop, one cycle, one node — go through the macros
// below, because there is no function boundary to hang an attribute on.
//
// Each macro expands to an RAII span guard bound to a `let`, so the span closes
// at the end of the enclosing block. With the feature off it expands to
// nothing at all, so an uninstrumented build carries no guard, no branch, and
// no `tracing` dependency.
// ---------------------------------------------------------------------------

/// Span covering one lifecycle phase (`start` / `stop` / `teardown`) applied
/// across every node — legacy's `apply_nodes` span, recording the phase in the
/// same `desc` field it does.
macro_rules! apply_nodes_span {
    ($desc:expr) => {
        #[cfg(feature = "instrument-apply-nodes")]
        let _apply_nodes_span = tracing::info_span!("apply_nodes", desc = $desc).entered();
    };
}

/// Span covering one engine cycle — legacy's `cycle` span. Entered by both
/// dispatch strategies (sparse drain and the full-sweep oracle) so the two are
/// indistinguishable to a subscriber.
macro_rules! cycle_span {
    () => {
        #[cfg(feature = "instrument-cycle")]
        let _cycle_span = tracing::info_span!("cycle").entered();
    };
}

/// Span covering a single node's `cycle` — legacy's `cycle_node` span.
/// Records the node index and its label (wingfoil's equivalent of legacy's
/// `type_name`). One span per node per cycle: high frequency, hence its own
/// opt-in feature.
macro_rules! cycle_node_span {
    ($index:expr, $label:expr) => {
        #[cfg(feature = "instrument-cycle-node")]
        let _cycle_node_span =
            tracing::info_span!("cycle_node", index = $index, node = $label).entered();
    };
}

/// Anything that identifies a node's typed output — a raw [`Handle`] or a
/// fluent [`Stream`](crate::fluent::Stream).
pub trait AsHandle<T> {
    fn as_handle(&self) -> Handle<T>;
}

/// A typed reference to a node's output within a [`Builder`] / [`Runner`].
pub struct Handle<T> {
    idx: usize,
    /// The id of the [`Builder`] that minted this handle (see
    /// [`NEXT_BUILDER_ID`]). Guards against using a handle with a different
    /// builder's runner (a colliding index + type would otherwise return the
    /// wrong node's value).
    builder_id: u64,
    _t: PhantomData<T>,
}

impl<T> AsHandle<T> for Handle<T> {
    fn as_handle(&self) -> Handle<T> {
        *self
    }
}

// Hand-written (not `#[derive]`) on purpose: a `Handle` is only an index +
// `PhantomData`, so it is `Copy` for *every* `T`. `#[derive(Clone, Copy)]`
// would emit `impl<T: Clone> …` / `impl<T: Copy> …`, adding a spurious bound
// on `T` that this type does not need (it stores no `T` by value). The same
// reasoning applies to the other manual `Clone` impls in this module
// (`Stream`, `ExternalSource`, `FeedbackSink`, `ChannelSender`).
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Handle<T> {}

impl<T> Handle<T> {
    /// The node index this handle refers to.
    #[doc(hidden)]
    pub fn index(&self) -> usize {
        self.idx
    }
}

/// A historical channel receiver's persistent read state: the time-grouped
/// look-ahead buffer, an end-of-stream flag, and the highest timestamp seen so
/// far (a group is known-complete once a strictly-later timestamp arrives or the
/// stream ends). Carried in the channel node's shared cell across cycles.
struct HistRead<T: Default> {
    groups: VecDeque<(NanoTime, Burst<T>)>,
    eof: bool,
    seen_max: Option<NanoTime>,
}

impl<T: Default> Default for HistRead<T> {
    fn default() -> Self {
        Self {
            groups: VecDeque::new(),
            eof: false,
            seen_max: None,
        }
    }
}

/// Incrementally drain a historical channel receiver into time-grouped
/// look-ahead bursts — the streaming counterpart of the old block-collect.
///
/// Reads messages until either every group with time `<= upto` is known
/// complete (a strictly-later timestamp has been seen, or the stream ended)
/// or — when `blocking` is `false` — the channel is momentarily empty. This is
/// the port of legacy `ChannelReceiverStream`'s "block while behind, stop once
/// caught up" loop (`legacy/wingfoil/src/nodes/channel.rs`): it holds at most one group
/// of look-ahead, so memory is bounded and a producer that depends on the
/// receiving graph's own output can make progress (the block-collect predecessor
/// deadlocked it, which is why `spawn_map`'s historical mode was impossible).
///
/// `state` is the receiver's persistent read state (see [`HistRead`]). Timestamps
/// must be `>= start_time` and non-decreasing (legacy errors on either); a
/// `Message::Error` aborts the run.
///
/// `read_past` distinguishes the two legacy drive modes. A **self-driven**
/// receiver reads *past* `upto` (`seen_max > upto`) so a same-time burst
/// delivered as separate messages is drained whole before delivery. A
/// **triggered** receiver (the `spawn_map` worker feed) stops as soon as it has
/// the value *at* `upto` (`seen_max >= upto`) and never reads ahead — reading
/// past would block on a value the producer cannot send until it receives more
/// input from this very graph, the deadlock the block-collect had.
///
/// Generic over a **transported → emitted** mapping: `R` is what rides the
/// channel, `E` is what the graph sees, and `wrap` converts each value as it
/// is buffered. Plain channels use the identity; `pooled_channel` transports
/// the unique [`PoolLoan`](crate::pool::PoolLoan) and wraps it into the
/// graph-thread-only [`Pooled`](crate::pool::Pooled) handle *here* — this
/// function only ever runs on the graph thread, which is what makes handing
/// out a non-`Send` handle sound.
fn pump_historical<R, E: Default>(
    rx: &std::sync::mpsc::Receiver<Message<R>>,
    state: &mut HistRead<E>,
    start_time: NanoTime,
    upto: NanoTime,
    read_past: bool,
    blocking: bool,
    wrap: &impl Fn(R) -> E,
) -> Result<()> {
    use std::sync::mpsc::TryRecvError;
    loop {
        if state.eof {
            break;
        }
        // Stop once everything at or before `upto` is buffered. Self-driven
        // reads one strictly-later timestamp to close a same-time burst;
        // triggered stops at the first message stamped `>= upto`.
        if let Some(mx) = state.seen_max
            && (if read_past { mx > upto } else { mx >= upto })
        {
            break;
        }
        let msg = if blocking {
            match rx.recv() {
                Ok(m) => m,
                // All senders dropped without an explicit close.
                Err(_) => {
                    state.eof = true;
                    break;
                }
            }
        } else {
            match rx.try_recv() {
                Ok(m) => m,
                // Nothing more buffered right now — a triggered receiver stops
                // here rather than blocking (that is what avoids the deadlock).
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    state.eof = true;
                    break;
                }
            }
        };
        let (t, v) = match msg {
            Message::ValueAt(v, t) => (t, v),
            // An unstamped value replays at the run's start time.
            Message::Value(v) => (start_time, v),
            // A progress marker carries no value; the historical block-collect
            // predecessor ignored it, so we do too (it neither ticks nor groups).
            Message::Checkpoint(_) => continue,
            Message::EndOfStream => {
                state.eof = true;
                break;
            }
            Message::Error(e) => {
                return Err(
                    anyhow::anyhow!("{e:#}").context("channel receiver: producer sent an error")
                );
            }
        };
        // Reject a pre-start timestamp: the kernel schedules callbacks verbatim,
        // so a time before `start_time` would rewind the run clock. Legacy
        // errors on any time behind the graph clock; we mirror that.
        if t < start_time {
            return Err(anyhow::anyhow!(
                "channel receiver: historical send_at time {t} is before the run start time \
                 {start_time} — timestamps must be at or after the start of the replay"
            ));
        }
        // Enforce non-decreasing send order (the graph clock only advances, so
        // an earlier timestamp is the equivalent of legacy's behind-the-clock
        // message).
        if let Some(mx) = state.seen_max
            && t < mx
        {
            return Err(anyhow::anyhow!(
                "channel receiver: historical send_at time {t} is out of order (after {mx}) — \
                 timestamped sends must be non-decreasing (legacy errors on out-of-order)"
            ));
        }
        state.seen_max = Some(t);
        let v = wrap(v);
        match state.groups.back_mut() {
            Some((bt, burst)) if *bt == t => burst.push(v),
            _ => state.groups.push_back((t, Burst::from([v]))),
        }
    }
    Ok(())
}

impl Builder {
    /// Mint a [`Handle`] stamped with this builder's id.
    pub(crate) fn make_handle<T>(&self, idx: usize) -> Handle<T> {
        Handle {
            idx,
            builder_id: self.id,
            _t: PhantomData,
        }
    }
}

/// The frozen access boundary between an op and the value store.
///
/// Every op registration closure reads and writes its slots **only** through a
/// `SlotRef` — `borrow()` for a shared read of the current value, `borrow_mut()`
/// for the write — never through the concrete backing cell. Today a `SlotRef`
/// wraps one `Rc<RefCell<T>>`, but because no capture site names that type, the
/// store can move to a contiguous arena / structure-of-arrays later (Phase 4.5)
/// as an internal swap of this struct's innards, without touching a single
/// registration or emission path. This is the "freeze the slot API boundary"
/// mitigation from the port plan, made *by type* rather than by convention.
///
/// Cheap to clone (clones the underlying handle), so a `move` cycle closure
/// captures its own `SlotRef` by value, exactly as it captured an `Rc` before.
///
/// `pub` + `#[doc(hidden)]` (like [`Stream::__slot`](crate::fluent::Stream::__slot)
/// and the other codegen hooks): the `nitro!` macro's `nested` expansion, which
/// lands in downstream crates, reads island inputs through `SlotRef::borrow`.
#[doc(hidden)]
pub struct SlotRef<T> {
    cell: Rc<RefCell<T>>,
}

// Hand-written `Clone` (not `#[derive]`) for the same reason as `Handle`: a
// `SlotRef` shares a handle to storage, so it is cloneable for *every* `T`,
// with no spurious `T: Clone` bound.
impl<T> Clone for SlotRef<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
        }
    }
}

impl<T> SlotRef<T> {
    fn new(cell: Rc<RefCell<T>>) -> Self {
        Self { cell }
    }

    /// Shared read of the slot's current value. `pub` (doc-hidden) because the
    /// `nitro!` `nested` expansion calls it from downstream crates.
    #[doc(hidden)]
    pub fn borrow(&self) -> Ref<'_, T> {
        self.cell.borrow()
    }

    /// Exclusive write access to the slot. Crate-internal — only op
    /// registration writes a slot; downstream codegen only ever reads.
    pub(crate) fn borrow_mut(&self) -> RefMut<'_, T> {
        self.cell.borrow_mut()
    }
}

/// Trim a `type_name` — `wingfoil::ops::Map<u64, …, {{closure}}>` — down
/// to the bare op name (`Map`) for error context: drop everything from the
/// first `<`, then keep only the final `::` segment. A plain label with no
/// path or generics passes through unchanged, so hand-written and
/// `#[op]`-generated nodes read the same in error messages.
pub(crate) fn short_type_name(s: &'static str) -> &'static str {
    let head = s.split('<').next().unwrap_or(s);
    head.rsplit("::").next().unwrap_or(head)
}

pub(crate) type CycleFn = Box<dyn FnMut(&mut Kernel) -> Result<bool>>;
/// Start / stop / teardown all share this shape.
pub(crate) type LifecycleFn = Box<dyn FnMut(&mut Kernel) -> Result<()>>;
/// A node's re-run hook: restore its engine-owned state and value slot to
/// their **wiring-time initial values**, so a second [`Runner::run`] starts
/// from a clean slate (the kernel is rebuilt from `t=0` each run, and each
/// node's `start` re-seeds its schedules, so this closure need only reset
/// per-node state — never the kernel). Takes no [`Kernel`]: reset happens
/// *between* runs, when no kernel exists. Restoring a stored value is
/// infallible, so unlike the other hooks it returns nothing. Defaults to a
/// no-op (a stateless node with no persistent slot needs no reset).
pub(crate) type ResetFn = Box<dyn FnMut()>;

/// A graph mutation staged by an in-graph node during its `cycle` (where it
/// cannot borrow the `Runner`), applied by [`Runner::run_dynamic`] at the next
/// cycle boundary. This is how a self-contained dynamic node (e.g. a
/// [`dynamic_group`](Builder::dynamic_group)) grows or shrinks the graph from
/// inside its own logic — mirroring legacy's `pending_additions` /
/// `pending_removals` (`graph.rs:369-392`). Shared via `Rc` between the staging
/// node and the runner.
#[cfg_attr(not(feature = "dynamic-graph"), allow(dead_code))]
type PendingMut = Box<dyn FnOnce(&mut Runner, &mut Kernel) -> Result<()>>;

/// One node's **r**un**t**ime record: everything the engine needs to schedule
/// and drive that node, kept in parallel `Vec`s indexed by node position (its
/// [`Handle`] index). It is the erased, uniform counterpart to a typed [`Op`]
/// — the op's concrete `Cfg`/`State`/value slot are captured *inside* the
/// `cycle` closure (so this struct stays non-generic and all nodes live in one
/// `Vec`), while the fields here are the engine-visible facts: what activates
/// the node, and its lifecycle hooks.
struct NodeRt {
    /// Indices of upstream nodes whose tick activates this one (the active
    /// edges). A cycle runs when any of these ticked — see the dispatch loop
    /// in [`Runner::run`].
    active_ups: Vec<usize>,
    /// Indices of upstream nodes this node **reads but is not triggered by**
    /// (the passive edges — `sample`'s data leg, a `bimap`/`trimap` inactive
    /// input). They never appear in `active_downs` (no tick propagates along
    /// them), but they *do* count toward the node's dispatch `layer`: a passive
    /// reader must run after the value it reads, exactly as an active one must.
    /// Legacy tracks the same fact (every upstream, active or passive, raises
    /// the layer — `graph.rs:794`, `fix_layers` at `graph.rs:1153`); the
    /// index-order engine got this for free from wiring order, but the layered
    /// engine (and dynamic `fix_layers`) needs it explicit.
    passive_ups: Vec<usize>,
    /// The op's `ACTIVATION` — this contract drives dispatch: nodes without
    /// `callback_activated()` skip the dirty check entirely, and `always`
    /// nodes are cycled unconditionally (busy-poll sources).
    activation: Activation,
    /// The op kind, for error context ("node 3 (TryMap) cycle: ..."). Derived
    /// from `type_name` (shortened) for `#[op]` nodes, a literal for the
    /// remaining hand-written ones.
    label: &'static str,
    cycle: CycleFn,
    start: LifecycleFn,
    stop: LifecycleFn,
    teardown: LifecycleFn,
    /// Re-run hook — restores this node's state + value slot to their
    /// wiring-time initial values before a second [`Runner::run`]. See
    /// [`ResetFn`]. Defaults to a no-op; stateful nodes overwrite it.
    reset: ResetFn,
}

/// Project one [`NodeRt`] into the public [`NodeInfo`] the introspection
/// surface exposes. Lives here rather than in [`crate::introspect`] because
/// `NodeRt` is private to this module — the introspect module owns the *types*
/// and the renderers, the engine owns the extraction.
fn node_info(index: usize, node: &NodeRt, removed: bool) -> NodeInfo {
    NodeInfo {
        index,
        label: node.label.to_string(),
        activation: node.activation,
        active_ups: node.active_ups.clone(),
        passive_ups: node.passive_ups.clone(),
        removed,
    }
}

/// The producer half of an [`external`](Builder::external) source: send a
/// value from any thread (or async task) and the kernel wakes to process it.
pub struct ExternalSource<T> {
    data: std::sync::mpsc::Sender<T>,
    waker: KernelWaker,
    index: usize,
}

impl<T> Clone for ExternalSource<T> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            waker: self.waker.clone(),
            index: self.index,
        }
    }
}

impl<T> ExternalSource<T> {
    /// Send a value into the graph and wake the kernel. Returns false once
    /// the runner is gone — producers can use this to stop.
    pub fn send(&self, value: T) -> bool {
        self.data.send(value).is_ok() && self.waker.wake(self.index)
    }
}

/// The write end of a [`feedback`](Builder::feedback) edge. Wiring
/// `stream.feedback(&sink)` (fluent) forwards `stream` unchanged while also
/// pushing each value onto the shared queue and scheduling the paired source
/// node to emit it on the *next* engine cycle (`+1`), which is what breaks
/// the dependency cycle: the source node has no upstreams, so the graph sees
/// no loop. Clone-able so one source can be fed from several sites.
///
/// Unlike legacy's `FeedbackSink::send(value, &mut GraphState)`, this type
/// exposes **no** public `send`: sending requires scheduling the paired source
/// node (`source`), which is a *different* node than the caller's. Legacy does
/// this through `GraphState::add_callback_for_node`, but wingfoil's op-facing
/// [`Ctx`](crate::op::Ctx) is deliberately narrow — self-scheduling only — and
/// cannot schedule an arbitrary node. Exposing a user-callable `send` would
/// need either a wider `Ctx` (against the design) or a kernel handle on the
/// sink; deferred until a concrete need arises. The `feedback_send` wiring
/// (fluent `stream.feedback(&sink)`) covers the pass-through case and does the
/// scheduling with direct kernel access.
pub struct FeedbackSink<T> {
    queue: Rc<RefCell<TimeQueue<T>>>,
    /// The paired source node's index, scheduled directly on the kernel — an
    /// engine-level edge the narrow `Ctx` (self-scheduling only) can't
    /// express.
    source: usize,
}

impl<T> Clone for FeedbackSink<T> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            source: self.source,
        }
    }
}

/// The teardown handle a [`source_at_start`](Builder::source_at_start) `setup`
/// returns. Whatever it wraps is held for the run and **dropped at teardown**,
/// so its `Drop` is where a deferred producer stops — the zmq adapter's
/// `ThreadStopGuard` pattern, generalised and made start-scoped. Wrap any
/// `'static` value whose `Drop` signals your producer to stop (a stop-flag
/// guard, a `JoinHandle` wrapper, an owned socket).
pub struct StopHandle(
    // Held only for its `Drop`; never read or downcast. Dropping the
    // `Box<dyn Any>` runs the wrapped guard's own `Drop`, which is what stops
    // the producer — `Any` (rather than, say, `Box<dyn FnOnce()>`) just so any
    // `'static` guard type can be wrapped without naming it.
    #[allow(dead_code)] Box<dyn Any>,
);

impl StopHandle {
    /// Wrap a teardown guard; its `Drop` runs when the run tears down.
    pub fn new(guard: impl Any) -> Self {
        Self(Box::new(guard))
    }
}

/// Graph-scoped ownership of the async runtime the async adapters spawn onto.
///
/// The executor-free core never names a runtime; all tokio-specific state lives
/// in [`GraphRuntime`](crate::async_source::GraphRuntime), reached only under the
/// `async` feature. The slot is a field on every [`Builder`]/[`Runner`] (so the
/// struct shape is uniform regardless of feature) but is inert — zero-sized and
/// doing nothing — unless `async` is on. When on, it holds the lazily-created
/// owned runtime, is carried from the builder into the [`Runner`] at
/// [`build`](Builder::build), and — because it is the **last** field of `Runner`
/// — is dropped only *after* every node, so an offloaded sink's teardown
/// `block_on` still has a live runtime. See `docs/decisions/runtime-ownership.md`.
#[derive(Default)]
pub(crate) struct AsyncRuntimeSlot {
    #[cfg(feature = "async")]
    inner: crate::async_source::GraphRuntime,
}

/// Wires a graph of [`Op`]s. Combinators mirror the legacy fluent API but
/// the engine — not the node — owns state, config and values.
pub struct Builder {
    nodes: Vec<NodeRt>,
    slots: Vec<Rc<dyn Any>>,
    ticked: Rc<RefCell<Vec<bool>>>,
    waker: KernelWaker,
    ready: Option<ReadyReceiver>,
    /// `external`/`poll` sources are wall-clock (realtime-only).
    has_external: bool,
    has_always: bool,
    /// `channel` sources carry timestamps, so they run in **both** modes:
    /// realtime (waker-driven) and historical (schedule-driven replay).
    has_channel: bool,
    /// Set by a channel node when it receives [`Message::EndOfStream`]
    /// (`close()`), so a realtime run ends even while a producer keeps a live
    /// [`ChannelSender`] clone — the kernel alone only ends the run when
    /// *every* waker clone is dropped. Mirrors legacy's per-receiver
    /// `finished` flag (here one shared flag ends the run on any channel
    /// close, which is the single-channel realtime case the fix targets).
    finished: Rc<Cell<bool>>,
    /// True while every node in the graph can restore itself for a re-run
    /// (see [`ResetFn`]). Cleared by nodes that hold state the engine cannot
    /// reset — `external`/`poll`/`channel` sources (their producer channels
    /// and wakers are consumed by the first run) and `composite` islands
    /// (their interior owns a private runtime with no reset hook). A
    /// [`Runner`] built from such a graph is single-run; a pure historical
    /// graph (tickers/constants + combinators + feedback) re-runs.
    re_runnable: bool,
    /// Process-unique id (see [`NEXT_BUILDER_ID`]), stamped on every [`Handle`]
    /// this builder mints and carried into its [`Runner`], so a handle used
    /// with a *different* runner is caught by a `debug_assert`.
    id: u64,
    /// Mutations staged by in-graph dynamic nodes (e.g. a
    /// [`dynamic_group`](Builder::dynamic_group)) during a cycle, applied at the
    /// cycle boundary by [`Runner::run_dynamic`]. Shared (`Rc`) with any such
    /// node at wiring time; empty and inert on a static graph.
    #[cfg_attr(not(feature = "dynamic-graph"), allow(dead_code))]
    pending: Rc<RefCell<Vec<PendingMut>>>,
    /// Same-cycle "mark this node dirty" requests from a routing node (a
    /// [`demux`](Builder::demux) parent), drained *within* the current cycle by
    /// the dispatch loop. Shared (`Rc`) with the routing node. Distinct from
    /// `pending` (next-cycle structural mutation) — this is fixed-topology
    /// dynamic *routing*: a node enqueues a chosen, already-wired downstream to
    /// fire this cycle. `has_marks` gates the drain off the hot path.
    marks: Rc<RefCell<Vec<usize>>>,
    /// Set when any [`demux`](Builder::demux) node is wired, so the dispatch
    /// loop knows to drain `marks`. Off (and the drain skipped) otherwise.
    has_marks: bool,
    /// Graph-owned async runtime (with caller override) for the async adapters.
    /// Inert unless the `async` feature is on; carried into the [`Runner`] at
    /// [`build`](Self::build). See [`AsyncRuntimeSlot`].
    async_rt: AsyncRuntimeSlot,
}

impl Default for Builder {
    fn default() -> Self {
        let (waker, ready) = waker_channel();
        Self {
            nodes: Vec::new(),
            slots: Vec::new(),
            ticked: Rc::default(),
            waker,
            ready: Some(ready),
            has_external: false,
            has_always: false,
            has_channel: false,
            finished: Rc::new(Cell::new(false)),
            re_runnable: true,
            id: NEXT_BUILDER_ID.fetch_add(1, Ordering::Relaxed),
            pending: Rc::new(RefCell::new(Vec::new())),
            marks: Rc::new(RefCell::new(Vec::new())),
            has_marks: false,
            async_rt: AsyncRuntimeSlot::default(),
        }
    }
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// An external source: values sent through the returned
    /// [`ExternalSource`] (from any thread or async task) tick this stream.
    /// Emits a [`Burst`] — **every** value that arrived since the last cycle,
    /// in order (never latest-wins, never dropped). Realtime only, single
    /// [`Runner::run`].
    pub fn external<T: Clone + Default + 'static>(
        &mut self,
    ) -> (Handle<Burst<T>>, ExternalSource<T>) {
        let idx = self.nodes.len();
        let out = self.new_slot(Burst::<T>::new());
        let (tx, rx) = std::sync::mpsc::channel::<T>();
        self.has_external = true;
        // The producer channel + waker are consumed by the first run.
        self.re_runnable = false;
        self.push_node(
            Vec::new(),
            Activation::THREADED,
            "external",
            Box::new(move |_k| {
                // Drain everything pending into one burst — no coalescing.
                let mut burst: Burst<T> = Burst::new();
                while let Ok(v) = rx.try_recv() {
                    burst.push(v);
                }
                if burst.is_empty() {
                    Ok(false)
                } else {
                    *out.borrow_mut() = burst;
                    Ok(true)
                }
            }),
            Box::new(|_| Ok(())),
        );
        let source = ExternalSource {
            data: tx,
            waker: self.waker.clone(),
            index: idx,
        };
        (self.make_handle(idx), source)
    }

    /// Open a channel: a source stream fed by the returned [`ChannelSender`]
    /// (moved to another thread or async task). Emits a [`Burst`] — every
    /// value at a given instant, grouped, never coalesced — and works in
    /// **both** run modes:
    ///
    /// - **Realtime**: each `send` wakes the kernel; a cycle emits a burst of
    ///   all values that arrived since the last one (wall-clock paced).
    /// - **Historical**: the producer sends timestamped values
    ///   ([`ChannelSender::send_at`]) then [`close`](ChannelSender::close);
    ///   the receiver collects them at `start`, groups same-timestamp values
    ///   into one burst, and schedules delivery on the graph clock — so they
    ///   replay **deterministically** at their timestamps regardless of
    ///   wall-clock arrival (the legacy `produce_async` model). Same-time
    ///   values ride one atomic burst, never split or dropped.
    ///
    /// A `Message::Error` propagates into the graph and aborts the run.
    pub fn channel<T: Clone + Default + 'static>(
        &mut self,
    ) -> (Handle<Burst<T>>, ChannelSender<T>) {
        self.channel_bounded(None)
    }

    /// [`channel`](Self::channel) with an optional transport bound. `None` is the
    /// unbounded default; `Some(n)` makes the producer→graph transport a bounded
    /// `sync_channel(n)`, so a producer sending faster than the graph drains
    /// **blocks** on `send` (back-pressure) instead of accumulating an unbounded
    /// backlog. **Do not** bound a producer that fills the buffer *before the run
    /// starts* (e.g. `replay_results`, which queues its whole feed at wiring —
    /// there is no running consumer to drain it, so a bounded send would block at
    /// wiring); those keep the unbounded path.
    pub fn channel_bounded<T: Clone + Default + 'static>(
        &mut self,
        buffer: Option<usize>,
    ) -> (Handle<Burst<T>>, ChannelSender<T>) {
        // Self-driven, read-ahead: an independent producer may batch several
        // values at one instant, so read one timestamp past `now` to close the
        // same-time burst before delivering it.
        self.channel_inner(None, true, buffer)
    }

    /// A [`channel`](Self::channel) whose payloads are **pooled**: the
    /// producer loans recycled buffers ([`PooledSender::loan`]), writes them
    /// in place and sends the unique owner; the receiver emits
    /// [`Pooled<T>`] handles — non-atomic shared references whose last drop
    /// returns the buffer to the pool. Payload allocations are zero at steady
    /// state; the bounded pool (`capacity` buffers) is the backpressure. See
    /// the [`pool`](crate::pool) module docs for the full design.
    ///
    /// `init` builds each pool buffer once, up front — use it to pre-size
    /// interior capacity (`Vec::with_capacity`) so even first uses avoid
    /// growth reallocation.
    ///
    /// Delivery semantics are exactly [`channel`](Self::channel)'s: burst
    /// grouping, both run modes (`send_at` + `close` for historical replay),
    /// error propagation. The transport is unbounded because the pool itself
    /// bounds values in flight. One caveat inherited from the bounded-channel
    /// rule: a **historical** producer that queues its whole feed before the
    /// run starts blocks on [`loan`](PooledSender::loan) once `capacity`
    /// values are outstanding — size the pool for the backlog, or produce
    /// concurrently with the run.
    pub fn pooled_channel<T, F>(
        &mut self,
        capacity: usize,
        init: F,
    ) -> (Handle<Burst<Pooled<T>>>, PooledSender<T>)
    where
        T: Send + 'static,
        F: Fn() -> T,
    {
        assert!(capacity > 0, "pooled_channel capacity must be at least 1");
        let (ret_tx, ret_rx) = std::sync::mpsc::channel::<Box<T>>();
        let free: Vec<Box<T>> = (0..capacity).map(|_| Box::new(init())).collect();
        // The wrap runs on the graph thread (see `channel_inner_mapped`), so
        // minting the non-`Send` handle there is sound. `release_quiet` is on:
        // this receiver's burst holds pool loans, and the slot must not park
        // them past a quiet wake (see the flag's doc).
        let (handle, chan) = self.channel_inner_mapped(
            None,
            true,
            None,
            true,
            Pooled::adopt as fn(PoolLoan<T>) -> Pooled<T>,
        );
        (handle, PooledSender::new(chan, free, ret_rx, ret_tx))
    }

    /// A channel receiver driven by an upstream `trigger` node rather than
    /// self-scheduling — the incremental, non-blocking counterpart used to feed
    /// a `spawn_map` worker's output back into the graph. In historical mode it
    /// reads only what is already due at the current time (blocking while behind
    /// to stay deterministic, never reading ahead), so a producer that depends on
    /// this graph's output — the worker — can make progress. The trigger must
    /// tick at (at least) every instant the receiver should deliver at, and
    /// should be sequenced *after* whatever sends the worker its input (wire the
    /// send sink as the trigger, so its lower layer runs first each cycle).
    pub(crate) fn channel_triggered<T: Clone + Default + 'static>(
        &mut self,
        trigger: usize,
        buffer: Option<usize>,
    ) -> (Handle<Burst<T>>, ChannelSender<T>) {
        self.channel_inner(Some(trigger), false, buffer)
    }

    /// A self-driven channel receiver that delivers one instant at a time with
    /// **no read-ahead** — for a producer feeding it in lock-step with this
    /// graph's clock (a `spawn_map` worker's *input*, fed one value per instant by
    /// the driving graph). Reading ahead would block on a value the producer
    /// cannot send until this graph advances, which it cannot while the reader
    /// blocks — the lock-step deadlock. Safe precisely because such a feed carries
    /// a single value per instant (no same-time burst to close).
    pub(crate) fn channel_lockstep<T: Clone + Default + 'static>(
        &mut self,
        buffer: Option<usize>,
    ) -> (Handle<Burst<T>>, ChannelSender<T>) {
        self.channel_inner(None, false, buffer)
    }

    fn channel_inner<T: Clone + Default + 'static>(
        &mut self,
        trigger: Option<usize>,
        read_ahead: bool,
        buffer: Option<usize>,
    ) -> (Handle<Burst<T>>, ChannelSender<T>) {
        self.channel_inner_mapped(trigger, read_ahead, buffer, false, std::convert::identity)
    }

    /// The generic channel-receiver core: `R` is the type transported over
    /// the producer channel, `E` the type the graph emits, `wrap` the per-value
    /// conversion — applied **on the graph thread** as values are buffered
    /// (both the historical pump and the realtime drain run there), which is
    /// what lets [`pooled_channel`](Self::pooled_channel) transport a `Send`
    /// unique loan and emit a non-`Send` shared handle. Plain channels pass
    /// the identity, which monomorphizes away.
    ///
    /// `release_quiet`: on a realtime wake that delivers nothing, overwrite a
    /// non-empty value slot with an empty burst. Plain channels keep the last
    /// burst readable (`false`); the pooled receiver sets it, because its
    /// burst *holds pool loans* — a flat-out producer can park the entire
    /// pool in the slot's last burst, and with `loan()` blocked no further
    /// send would ever overwrite it (the wedge `PooledSender::loan`'s
    /// checkpoint nudge + this release together break).
    fn channel_inner_mapped<R, E, W>(
        &mut self,
        trigger: Option<usize>,
        read_ahead: bool,
        buffer: Option<usize>,
        release_quiet: bool,
        wrap: W,
    ) -> (Handle<Burst<E>>, ChannelSender<R>)
    where
        R: 'static,
        E: Default + 'static,
        W: Fn(R) -> E + Clone + 'static,
    {
        let triggered = trigger.is_some();
        let idx = self.nodes.len();
        let out = self.new_slot(Burst::<E>::new());
        // Unbounded by default; `Some(n)` bounds the transport to `sync_channel(n)`
        // so a producer outrunning the graph blocks on `send` (back-pressure).
        let (tx, rx) = match buffer {
            None => {
                let (tx, rx) = std::sync::mpsc::channel::<Message<R>>();
                (Tx::Unbounded(tx), rx)
            }
            Some(n) => {
                let (tx, rx) = std::sync::mpsc::sync_channel::<Message<R>>(n);
                (Tx::Bounded(tx), rx)
            }
        };
        self.has_channel = true;
        // The receiver is drained (historical) or waker-driven (realtime) by
        // the first run; a second run would see an empty channel.
        self.re_runnable = false;
        // Shared between the cycle and start adapters: the receiver plus the
        // incremental historical read state (see `HistRead`). One-ahead, not
        // block-collect (see `pump_historical`).
        let cs = Self::cell(rx, HistRead::<E>::default());
        let wrap_start = wrap.clone();
        let cs2 = cs.clone();
        let finished = self.finished.clone();
        self.push_node(
            trigger.map(|t| vec![t]).unwrap_or_default(),
            Activation {
                // A self-driven receiver re-arms its own callbacks; a triggered
                // one is fired by its upstream (and a realtime waker), so it does
                // not schedule.
                schedules: !triggered,
                threaded: true,
                always: false,
            },
            if triggered {
                "channel(triggered)"
            } else {
                "channel"
            },
            Box::new(move |k| {
                match k.run_mode() {
                    // Historical: read incrementally up to the current time
                    // (block while behind so a same-time burst arrives whole),
                    // deliver the group due now, and — when self-driven — re-arm
                    // at the next group's timestamp. Never block-collect.
                    RunMode::HistoricalFrom(_) => {
                        let now = k.time();
                        let start_time = k.start_time();
                        let (rx, state) = &mut *cs.borrow_mut();
                        // `read_ahead` false: stop at the value due now, never
                        // read ahead (reading ahead would block on data the
                        // producer cannot send until this graph advances — the
                        // lock-step deadlock).
                        pump_historical(rx, state, start_time, now, read_ahead, true, &wrap)?;
                        let ticked = match state.groups.front() {
                            Some((t, _)) if *t <= now => {
                                let (_, burst) = state.groups.pop_front().expect("front checked");
                                *out.borrow_mut() = burst;
                                true
                            }
                            _ => false,
                        };
                        // Self-driven only: re-arm at the next buffered group's
                        // (always future) time; if none is buffered but the stream
                        // is still open, wake at `now` so the next cycle blocks for
                        // the next message. A triggered receiver is re-fired by its
                        // upstream instead.
                        if !triggered {
                            match state.groups.front() {
                                Some((t, _)) => k.schedule(idx, *t),
                                // Nothing buffered ahead. If the stream is still
                                // open, re-arm at `now` (the kernel bumps it one
                                // tick forward) so the next cycle blocks for the
                                // next message — legacy's caught-up
                                // `add_callback(now)` (channel.rs:238). A closed
                                // (eof) stream just winds down.
                                None => {
                                    if !state.eof {
                                        k.schedule(idx, now);
                                    }
                                }
                            }
                        }
                        Ok(ticked)
                    }
                    // Realtime: drain everything pending into one burst.
                    RunMode::RealTime => {
                        let (rx, _) = &mut *cs.borrow_mut();
                        let mut burst: Burst<E> = Burst::new();
                        loop {
                            match rx.try_recv() {
                                Ok(Message::Value(v) | Message::ValueAt(v, _)) => {
                                    burst.push(wrap(v))
                                }
                                Ok(Message::Error(e)) => {
                                    return Err(anyhow::anyhow!("{e:#}")
                                        .context("channel receiver: producer sent an error"));
                                }
                                // `close()` ends the run even while a producer
                                // keeps a live sender clone (the kernel alone
                                // waits for every waker to drop). We keep
                                // draining so any values queued *before* the
                                // close still ride this final burst.
                                Ok(Message::EndOfStream) => finished.set(true),
                                // A progress marker with no value: nothing to
                                // add to the burst. Realtime dispatch is
                                // waker-driven, so it needs no clock nudge —
                                // documented as a no-op here (contrast the
                                // historical receiver, which could schedule a
                                // wakeup at the checkpoint time).
                                Ok(Message::Checkpoint(_)) => {}
                                Err(_) => break,
                            }
                        }
                        if burst.is_empty() {
                            // A quiet wake. For a pooled receiver, drop the
                            // parked previous burst so its loans return to
                            // the pool — this is the wake the self-schedule
                            // below (and `PooledSender`'s exhausted-pool
                            // checkpoint nudge) exists to cause.
                            if release_quiet && !out.borrow().is_empty() {
                                *out.borrow_mut() = Burst::new();
                            }
                            Ok(false)
                        } else {
                            *out.borrow_mut() = burst;
                            if release_quiet {
                                // Self-arm a quiet wake so the burst just
                                // parked is released on the very next cycle
                                // rather than only when new data (which may
                                // itself be blocked on the loans this burst
                                // holds) overwrites it. The checkpoint nudge
                                // alone is racy: it can coalesce into a
                                // value-carrying cycle and be consumed
                                // without ever observing a quiet wake.
                                k.schedule(idx, k.time());
                            }
                            Ok(true)
                        }
                    }
                }
            }),
            Box::new(move |k| {
                // Historical, self-driven only: seed the incremental read. Read
                // just far enough to learn the first group's timestamp, then
                // schedule the first wakeup at it; per-cycle `pump_historical`
                // streams the rest on demand. No block-collect — bounded memory,
                // and a producer that depends on this graph's output still gets to
                // run (the deadlock the old block-collect documented is gone).
                //
                // A triggered receiver seeds nothing at start: it must not read
                // before its trigger has sent the worker any input, and it is
                // driven forward by that trigger, not by self-scheduled callbacks.
                if !triggered && let RunMode::HistoricalFrom(_) = k.run_mode() {
                    let start_time = k.start_time();
                    let (rx, state) = &mut *cs2.borrow_mut();
                    pump_historical(
                        rx,
                        state,
                        start_time,
                        start_time,
                        read_ahead,
                        true,
                        &wrap_start,
                    )?;
                    if let Some((t, _)) = state.groups.front() {
                        k.schedule(idx, *t);
                    }
                }
                Ok(())
            }),
        );
        let sender = ChannelSender::new(tx, self.waker.clone(), idx);
        (self.make_handle(idx), sender)
    }

    /// A [`channel`](Self::channel)-fed source whose producer is established in
    /// `start()` rather than at wiring — the **deferred-connection** primitive.
    ///
    /// The factory allocates the channel and stores `setup`, but performs **no**
    /// I/O: no thread is spawned and no socket is connected while the graph is
    /// merely being constructed. On each run, `start()` calls `setup`, handing
    /// it the [`ChannelSender`]; `setup` connects/spawns the live producer and
    /// returns a [`StopHandle`] that is dropped at teardown (running its `Drop`),
    /// so the producer is torn down with the run (the `ThreadStopGuard` pattern,
    /// generalised). A `setup` error aborts the run at start with node context —
    /// legacy-consistent: a connection failure surfaces when the run begins,
    /// not at construction.
    ///
    /// This is what lets an adapter's wiring stay side-effect-free and
    /// unit-testable: address parsing, registry lookup and historical-mode
    /// rejection are pure `Result`-returning work done *before* calling this;
    /// the socket/thread only exist during a run.
    ///
    /// Delivery reuses [`channel`](Self::channel), so the same realtime/historical
    /// burst semantics apply. A *live* deferred source (a subscriber socket, say)
    /// is realtime-only and its caller should reject historical at wiring, exactly
    /// as [`channel`](Self::channel) callers do today. The receive channel and
    /// waker are consumed by the first run, so — like `channel` — a graph built
    /// with this source is single-run for now (re-run is a documented follow-on;
    /// see `docs/decisions/source-lifecycle.md`).
    ///
    /// **A historical producer must `close()` explicitly.** Unlike
    /// [`channel`](Self::channel) — whose sender is handed to the caller — this
    /// source *retains* a live [`ChannelSender`] for the whole run (it clones one
    /// into `setup` at each start). A historical up-front collect therefore ends
    /// only on an explicit `close()` / `EndOfStream`, never on the
    /// all-senders-dropped path (the retained sender keeps the channel open), so
    /// a producer that finishes by merely dropping its sender would deadlock the
    /// collect. Realtime live sources — the primary use — are unaffected: they
    /// terminate on `close()` or the run bound.
    pub fn source_at_start<T, Setup>(&mut self, mut setup: Setup) -> Handle<Burst<T>>
    where
        T: Clone + Default + 'static,
        Setup: FnMut(ChannelSender<T>) -> Result<StopHandle> + 'static,
    {
        self.source_at_start_with_params(None, move |sender, _run_mode, _run_for, _start_time| {
            setup(sender)
        })
    }

    /// Like [`source_at_start`](Self::source_at_start), but the `setup` closure
    /// is also handed the run's [`RunMode`]/[`RunFor`]/`start_time` at start — the
    /// bounds a producer needs when it must itself run a *sub-graph* under the
    /// same bound. This is what [`spawn`](crate::fluent::SourceOps::spawn) rides:
    /// its worker thread builds and runs its own graph and can only learn the run
    /// parameters here (they do not exist at wiring). The composed `start`/
    /// `teardown` semantics are identical to `source_at_start`.
    pub(crate) fn source_at_start_with_params<T, Setup>(
        &mut self,
        buffer: Option<usize>,
        setup: Setup,
    ) -> Handle<Burst<T>>
    where
        T: Clone + Default + 'static,
        Setup: FnMut(ChannelSender<T>, RunMode, RunFor, NanoTime) -> Result<StopHandle> + 'static,
    {
        let (handle, sender) = self.channel_bounded::<T>(buffer);
        let idx = handle.idx;
        let node = &mut self.nodes[idx];
        // The channel node already carries a `start` hook (the historical
        // collect). Compose the deferred `setup` **ahead** of it: `setup` spawns
        // the producer first, so a historical deferred source's up-front collect
        // sees a live producer instead of deadlocking on an unstarted one. For a
        // realtime live source the collect branch is a no-op, so the order is
        // moot there.
        let mut prev_start = std::mem::replace(&mut node.start, Box::new(|_| Ok(())));
        // Compose (don't clobber) the channel node's teardown as well: it is a
        // no-op today, but replacing it outright would silently drop any teardown
        // hook `channel` grows later. Our guard-drop runs first, then the
        // previous teardown — symmetric with the `start` composition above.
        let mut prev_teardown = std::mem::replace(&mut node.teardown, Box::new(|_| Ok(())));
        let mut setup = setup;
        // Holds the `setup`-returned guard for the duration of the run; the
        // teardown hook drops it to stop the producer.
        let stop: Rc<RefCell<Option<StopHandle>>> = Rc::new(RefCell::new(None));
        let stop_at_start = stop.clone();
        node.start = Box::new(move |k| {
            let guard = setup(sender.clone(), k.run_mode(), k.run_for(), k.start_time())?;
            *stop_at_start.borrow_mut() = Some(guard);
            prev_start(k)
        });
        node.teardown = Box::new(move |k| {
            // Drop the StopHandle → its `Drop` signals the producer to stop,
            // then run the channel node's own teardown.
            stop.borrow_mut().take();
            prev_teardown(k)
        });
        handle
    }

    /// Compose a worker-spawn onto an **existing** node's `start`/`teardown`
    /// (unlike [`source_at_start_with_params`](Self::source_at_start_with_params),
    /// which mints a fresh channel source). At run start `setup` is handed the run
    /// params and returns a [`StopHandle`] held for the run and dropped at
    /// teardown. Used by [`spawn_map`](crate::fluent::StreamOps::spawn_map) to
    /// launch its worker off the *output receiver* node once the run bounds are
    /// known. `idx` must be a valid node index in this builder.
    pub(crate) fn compose_spawn_at_start<S>(&mut self, idx: usize, setup: S)
    where
        S: FnMut(RunMode, RunFor, NanoTime) -> Result<StopHandle> + 'static,
    {
        let node = &mut self.nodes[idx];
        let mut prev_start = std::mem::replace(&mut node.start, Box::new(|_| Ok(())));
        let mut prev_teardown = std::mem::replace(&mut node.teardown, Box::new(|_| Ok(())));
        let mut setup = setup;
        let stop: Rc<RefCell<Option<StopHandle>>> = Rc::new(RefCell::new(None));
        let stop_at_start = stop.clone();
        node.start = Box::new(move |k| {
            let guard = setup(k.run_mode(), k.run_for(), k.start_time())?;
            *stop_at_start.borrow_mut() = Some(guard);
            prev_start(k)
        });
        node.teardown = Box::new(move |k| {
            stop.borrow_mut().take();
            prev_teardown(k)
        });
    }

    /// Resolve the tokio runtime [`Handle`](tokio::runtime::Handle) the async
    /// adapters spawn onto: the caller override if one was installed via
    /// [`set_async_runtime_override`](Self::set_async_runtime_override),
    /// otherwise a runtime this graph creates lazily on first use and owns for
    /// its lifetime (one per graph — every async adapter shares it). See
    /// `docs/decisions/runtime-ownership.md`.
    #[cfg(feature = "async")]
    pub fn async_runtime_handle(&mut self) -> Result<tokio::runtime::Handle> {
        self.async_rt.inner.handle()
    }

    /// Install a caller-supplied runtime handle as the override, so the async
    /// adapters spawn onto the caller's runtime instead of the graph's own. The
    /// escape hatch for embedding in an existing async application or a custom
    /// runtime configuration.
    #[cfg(feature = "async")]
    pub fn set_async_runtime_override(&mut self, handle: tokio::runtime::Handle) {
        self.async_rt.inner.set_override(handle);
    }

    /// The index the *next* [`push_node`](Self::push_node) will occupy — the
    /// node's own index, which its `cycle` closure needs to build a [`Ctx`].
    /// Part of the `#[op]` wiring seam (see [`push_node`](Self::push_node)).
    pub(crate) fn next_node_index(&self) -> usize {
        self.nodes.len()
    }

    /// The shared per-node tick flags for the cycle in progress, indexed by
    /// node index. Part of the `#[op]` wiring seam: an op whose `In` shape
    /// carries an edge's tick flag (`delay`, `merge`) reads it from here.
    pub(crate) fn ticked_flags(&self) -> Rc<RefCell<Vec<bool>>> {
        self.ticked.clone()
    }

    pub(crate) fn slot<T: 'static>(&self, h: Handle<T>) -> SlotRef<T> {
        debug_assert_eq!(
            h.builder_id, self.id,
            "Handle used with a different Builder than the one that minted it"
        );
        SlotRef::new(
            self.slots[h.idx]
                .clone()
                .downcast::<RefCell<T>>()
                .expect("invariant: Handle<T> indexes a slot of type T"),
        )
    }

    pub(crate) fn new_slot<T: 'static>(&mut self, init: T) -> SlotRef<T> {
        let cell = Rc::new(RefCell::new(init));
        self.slots.push(cell.clone() as Rc<dyn Any>);
        SlotRef::new(cell)
    }

    /// Register a node: its slot must already have been pushed (so slot and
    /// node indices stay aligned). `stop`/`teardown` default to no-ops; a node
    /// that needs them (e.g. `finally`) attaches one with
    /// [`set_stop`](Self::set_stop) / [`set_teardown`](Self::set_teardown)
    /// right after pushing.
    ///
    /// This — with `next_node_index` / `new_slot` / `slot` / `ticked_flags`
    /// and the three `set_*` attachers — is the **`#[op]` wiring seam**: the
    /// `Builder` method `#[op(build = …)]` generates is exactly this sequence,
    /// which is why these are `pub(crate)` rather than private. The generated
    /// code lives in this crate (it names `crate::interp::Builder`), so the
    /// seam never widens past it; third-party ops wire through the public
    /// [`register_op1`](Self::register_op1)…[`register_op4`](Self::register_op4)
    /// primitives instead.
    pub(crate) fn push_node(
        &mut self,
        active_ups: Vec<usize>,
        activation: Activation,
        label: &'static str,
        cycle: CycleFn,
        start: LifecycleFn,
    ) {
        self.nodes.push(NodeRt {
            active_ups,
            passive_ups: Vec::new(),
            activation,
            label,
            cycle,
            start,
            stop: Box::new(|_| Ok(())),
            teardown: Box::new(|_| Ok(())),
            reset: Box::new(|| {}),
        });
        self.ticked.borrow_mut().push(false);
    }

    /// Attach a re-run hook to the node most recently pushed. Called by the
    /// registration methods right after `push_node`, so a second
    /// [`Runner::run`] restores that node's state and value slot. See
    /// [`ResetFn`].
    pub(crate) fn set_reset(&mut self, reset: ResetFn) {
        self.nodes
            .last_mut()
            .expect("invariant: set_reset called immediately after push_node")
            .reset = reset;
    }

    /// Attach a `stop` hook to the node most recently pushed — the end-of-run
    /// hook that still sees the last cycle's state (`timed`'s summary). Part
    /// of the `#[op]` wiring seam; emitted only for ops that override
    /// [`Op::stop`](crate::op::Op::stop).
    pub(crate) fn set_stop(&mut self, stop: LifecycleFn) {
        self.nodes
            .last_mut()
            .expect("invariant: set_stop called immediately after push_node")
            .stop = stop;
    }

    /// Attach a `teardown` hook to the node most recently pushed — the hook
    /// that runs after the run ends *even if a cycle aborted it* (`finally`).
    /// Part of the `#[op]` wiring seam; emitted only for ops that override
    /// [`Op::teardown`](crate::op::Op::teardown).
    pub(crate) fn set_teardown(&mut self, teardown: LifecycleFn) {
        self.nodes
            .last_mut()
            .expect("invariant: set_teardown called immediately after push_node")
            .teardown = teardown;
    }

    /// Record the passive upstream edges of the node at `idx` (the node just
    /// pushed by [`push_node`](Self::push_node)). Passive edges are read but do
    /// not trigger, so they stay out of `active_ups`/`active_downs`; they are
    /// tracked only so `build()` (and dynamic `fix_layers`) can raise the
    /// node's dispatch `layer` above every value it reads. Called by the
    /// `bimap` / `trimap` family (whose active/passive split is a runtime
    /// argument) and by the `#[op]`-generated wiring of any op declaring a
    /// `passive = [..]` mask (`sample`, `join_passive`); all-active shapes
    /// leave it empty.
    pub(crate) fn set_passive_ups(&mut self, idx: usize, passive_ups: Vec<usize>) {
        self.nodes[idx].passive_ups = passive_ups;
    }

    /// Shared cfg+state cell, used by both the cycle and start adapters.
    fn cell<C: 'static, S: 'static>(cfg: C, state: S) -> Rc<RefCell<(C, S)>> {
        Rc::new(RefCell::new((cfg, state)))
    }

    /// Register a **single-active-input** op — the shape shared by `map`,
    /// `fold`, `ewma`, and ~15 others: one upstream read by reference, one
    /// output slot, engine-owned `cfg`+`state`, no lifecycle hooks. Public so
    /// third-party op traits can wire this shape through
    /// [`Stream::wire`](crate::fluent::Stream::wire). This is the
    /// reusable core the `#[op]` attribute generates a thin wrapper around; the
    /// per-op `step` closure (which builds the concrete `(&a,)` input tuple and
    /// calls `Op::cycle`) is the only monomorphic piece, so this primitive
    /// stays free of the GAT-over-HRTB gymnastics a fully generic version would
    /// need. `label` is `type_name::<Op>()`; it is shortened for error context.
    ///
    /// `state_init` is a *factory* (not a value) so the engine can re-run it
    /// on a second [`Runner::run`], restoring the op's state to its
    /// wiring-time initial value (see [`ResetFn`]). The `#[op]`-generated
    /// wrapper passes `|| Default::default()`.
    pub fn register_op1<A, C, S, Out, Step, SInit>(
        &mut self,
        src: Handle<A>,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state_init: SInit,
        step: Step,
    ) -> Handle<Out>
    where
        A: 'static,
        C: 'static,
        S: 'static,
        Out: Default + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut C, &mut S, &A, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        self.register_op1_cell(src, label, activation, cfg, state_init, step)
            .0
    }

    /// The body of [`register_op1`](Self::register_op1), additionally handing
    /// back the shared cfg+state cell so
    /// [`register_op1_with_stop`](Self::register_op1_with_stop) can give its
    /// lifecycle hook the same view of them the cycle has.
    #[allow(clippy::type_complexity)]
    fn register_op1_cell<A, C, S, Out, Step, SInit>(
        &mut self,
        src: Handle<A>,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state_init: SInit,
        mut step: Step,
    ) -> (Handle<Out>, Rc<RefCell<(C, S)>>)
    where
        A: 'static,
        C: 'static,
        S: 'static,
        Out: Default + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut C, &mut S, &A, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(Out::default());
        let cs = Rc::new(RefCell::new((cfg, state_init())));
        let (cs_reset, out_reset, cs_out) = (cs.clone(), out.clone(), cs.clone());
        self.push_node(
            vec![src.idx],
            activation,
            short_type_name(label),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match step(cfg, state, &a, &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            cs_reset.borrow_mut().1 = state_init();
            *out_reset.borrow_mut() = Out::default();
        }));
        (self.make_handle(idx), cs_out)
    }

    /// [`register_op1`](Self::register_op1) plus a **`stop` hook** — the shape a
    /// sink with an end-of-run summary needs (`latency_report` prints one).
    ///
    /// Public because `set_stop` is not: an op wired from outside the `#[op]`
    /// macro (the erased Python boundary registers its ops through
    /// [`Stream::wire`](crate::fluent::Stream::wire), where the stage list is a
    /// runtime value and no `Op` impl exists to carry the hook) otherwise has no
    /// way to run anything after the last cycle. `stop` sees the same
    /// `cfg`/`state` the cycle does, and runs once when the run ends normally.
    #[allow(clippy::too_many_arguments)]
    pub fn register_op1_with_stop<A, C, S, Out, Step, SInit, Stop>(
        &mut self,
        src: Handle<A>,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state_init: SInit,
        step: Step,
        mut stop: Stop,
    ) -> Handle<Out>
    where
        A: 'static,
        C: 'static,
        S: 'static,
        Out: Default + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut C, &mut S, &A, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
        Stop: FnMut(&mut C, &mut S, &mut Ctx<'_>) -> Result<()> + 'static,
    {
        let idx = self.nodes.len();
        let (handle, cs) = self.register_op1_cell(src, label, activation, cfg, state_init, step);
        self.set_stop(Box::new(move |k| {
            let (cfg, state) = &mut *cs.borrow_mut();
            let mut ctx = Ctx::new(k, idx);
            stop(cfg, state, &mut ctx)
        }));
        handle
    }

    /// Register a **two-active-input** op — the `join` shape: both upstreams
    /// read by reference, both triggering, one output slot, engine-owned
    /// `cfg`+`state`, no lifecycle hooks. Public for the same reason as
    /// [`register_op1`](Self::register_op1): third-party op traits wire this
    /// shape through [`Stream::wire`](crate::fluent::Stream::wire) (passive
    /// edges keep hand-written methods — see [`bimap`](Self::bimap)).
    // One over clippy's limit, but this is a registration primitive whose
    // arguments mirror `register_op1` plus the second input handle — grouping
    // them into a struct would only move the eight names one level down.
    #[allow(clippy::too_many_arguments)]
    pub fn register_op2<A, B, C, S, Out, Step, SInit>(
        &mut self,
        a: Handle<A>,
        b: Handle<B>,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state_init: SInit,
        mut step: Step,
    ) -> Handle<Out>
    where
        A: 'static,
        B: 'static,
        C: 'static,
        S: 'static,
        Out: Default + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut C, &mut S, &A, &B, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let out = self.new_slot(Out::default());
        let cs = Rc::new(RefCell::new((cfg, state_init())));
        let (cs_reset, out_reset) = (cs.clone(), out.clone());
        self.push_node(
            vec![a.idx, b.idx],
            activation,
            short_type_name(label),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                match step(cfg, state, &va, &vb, &mut ctx)? {
                    Tick::Value(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            cs_reset.borrow_mut().1 = state_init();
            *out_reset.borrow_mut() = Out::default();
        }));
        self.make_handle(idx)
    }

    /// Register a **three-active-input** op — the `join3` shape: all three
    /// upstreams read by reference, all triggering, one output slot,
    /// engine-owned `cfg`+`state`, no lifecycle hooks. The three-input
    /// counterpart to [`register_op2`](Self::register_op2), and the general
    /// form of [`trimap`](Self::trimap) (which is specialised to `Join3`'s
    /// `Cfg = F, State = ()`).
    // As `register_op2`: over clippy's limit because it is a registration
    // primitive whose arguments mirror the smaller arities plus one handle.
    #[allow(clippy::too_many_arguments)]
    pub fn register_op3<A, B, C, Cfg, S, Out, Step, SInit>(
        &mut self,
        a: Handle<A>,
        b: Handle<B>,
        c: Handle<C>,
        label: &'static str,
        activation: Activation,
        cfg: Cfg,
        state_init: SInit,
        mut step: Step,
    ) -> Handle<Out>
    where
        A: 'static,
        B: 'static,
        C: 'static,
        Cfg: 'static,
        S: 'static,
        Out: Default + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut Cfg, &mut S, &A, &B, &C, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let c_slot = self.slot(c);
        let out = self.new_slot(Out::default());
        let cs = Rc::new(RefCell::new((cfg, state_init())));
        let (cs_reset, out_reset) = (cs.clone(), out.clone());
        self.push_node(
            vec![a.idx, b.idx, c.idx],
            activation,
            short_type_name(label),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                let vc = c_slot.borrow();
                match step(cfg, state, &va, &vb, &vc, &mut ctx)? {
                    Tick::Value(v) => {
                        drop((va, vb, vc));
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop((va, vb, vc));
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            cs_reset.borrow_mut().1 = state_init();
            *out_reset.borrow_mut() = Out::default();
        }));
        self.make_handle(idx)
    }

    /// Register a **four-active-input** op: all four upstreams read by
    /// reference, all triggering, one output slot, engine-owned `cfg`+`state`,
    /// no lifecycle hooks. The next rung up from
    /// [`register_op3`](Self::register_op3).
    ///
    /// Each arity is its own function because the inputs are *heterogeneous* —
    /// `&A, &B, &C, &D` are distinct static types the step closure is
    /// monomorphized over, and Rust has no variadic generics to fold them into
    /// one signature. A wider op adds the next rung the same way.
    // As the smaller arities: over clippy's limit because it is a registration
    // primitive whose arguments mirror them plus one handle.
    #[allow(clippy::too_many_arguments)]
    pub fn register_op4<A, B, C, D, Cfg, S, Out, Step, SInit>(
        &mut self,
        a: Handle<A>,
        b: Handle<B>,
        c: Handle<C>,
        d: Handle<D>,
        label: &'static str,
        activation: Activation,
        cfg: Cfg,
        state_init: SInit,
        mut step: Step,
    ) -> Handle<Out>
    where
        A: 'static,
        B: 'static,
        C: 'static,
        D: 'static,
        Cfg: 'static,
        S: 'static,
        Out: Default + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut Cfg, &mut S, &A, &B, &C, &D, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let c_slot = self.slot(c);
        let d_slot = self.slot(d);
        let out = self.new_slot(Out::default());
        let cs = Rc::new(RefCell::new((cfg, state_init())));
        let (cs_reset, out_reset) = (cs.clone(), out.clone());
        self.push_node(
            vec![a.idx, b.idx, c.idx, d.idx],
            activation,
            short_type_name(label),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                let vc = c_slot.borrow();
                let vd = d_slot.borrow();
                match step(cfg, state, &va, &vb, &vc, &vd, &mut ctx)? {
                    Tick::Value(v) => {
                        drop((va, vb, vc, vd));
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop((va, vb, vc, vd));
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            cs_reset.borrow_mut().1 = state_init();
            *out_reset.borrow_mut() = Out::default();
        }));
        self.make_handle(idx)
    }

    /// A source that never ticks (the legacy `never`). No upstreams and no
    /// scheduling, so the engine never cycles it; used as an inert trigger.
    pub fn never(&mut self) -> Handle<()> {
        let idx = self.nodes.len();
        // The slot is kept for index alignment; `never` never writes it.
        let _out = self.new_slot(());
        self.push_node(
            Vec::new(),
            Never::ACTIVATION,
            "never",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                match Never::cycle(&mut (), &mut (), (), &mut ctx)? {
                    Tick::Value(()) | Tick::Silent(()) => Ok(false),
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.make_handle(idx)
    }

    /// Combine several same-type streams into a `Stream<Burst<T>>` (the legacy
    /// `combine`): each cycle gathers the current values of every upstream that
    /// ticked *this* instant into one [`Burst`], in upstream order. Quiet on a
    /// cycle where none ticked (only reachable via a shared scheduled wake).
    ///
    /// Hand-written wiring, as [`merge_n`](Self::merge_n) is: a variadic fan-in
    /// does not fit the `Op` trait's fixed-arity tuple `In`, so `#[op]` cannot
    /// generate it. The *semantics* live in [`CombineN`], which the
    /// `nitro!`/compiled path drives; this path does not build the
    /// `&[(&T, bool)]` slice that op's `cycle` takes, because that means
    /// holding a borrow guard for every upstream slot at once — a per-cycle
    /// allocation on a wide fan-in. It walks the tick flags, borrows only the
    /// slots that ticked, and routes the outcome through the shared
    /// [`CombineN::emit`] rule so the two paths cannot disagree about the
    /// empty-burst case.
    ///
    /// Unlike the legacy port's shared `Rc<RefCell<Burst>>` cell written by
    /// per-stream feeder nodes, the burst is built locally here, honouring
    /// wingfoil's no-shared-mutable-slot rule.
    pub fn combine<T: Clone + Default + 'static>(
        &mut self,
        srcs: &[Handle<T>],
    ) -> Handle<Burst<T>> {
        let idx = self.nodes.len();
        let indices: Vec<usize> = srcs.iter().map(|h| h.idx).collect();
        let slots: Vec<SlotRef<T>> = srcs.iter().map(|h| self.slot(*h)).collect();
        let out = self.new_slot(Burst::<T>::new());
        let out_reset = out.clone();
        let ticked = self.ticked.clone();
        self.push_node(
            indices.clone(),
            Activation::NONE,
            "combine",
            Box::new(move |_k| {
                let mut burst = Burst::<T>::new();
                {
                    let t = ticked.borrow();
                    for (i, slot) in indices.iter().zip(slots.iter()) {
                        if t[*i] {
                            burst.push(slot.borrow().clone());
                        }
                    }
                }
                match CombineN::<T>::emit(burst) {
                    Tick::Value(burst) => {
                        *out.borrow_mut() = burst;
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = Burst::new();
        }));
        self.make_handle(idx)
    }

    /// Merge N same-type streams into one — the n-ary [`MergeN`] node backing
    /// `merge_all` / `fan`. The earliest-supplied upstream that ticked this
    /// cycle wins; quiet when none ticked.
    ///
    /// Hand-written for the same reason as [`combine`](Self::combine): a
    /// variadic fan-in does not fit the `Op` trait's fixed-arity tuple `In`,
    /// so `#[op]` cannot generate the wiring. Unlike the generated shapes this
    /// does **not** hand `Op::cycle` a materialised `&[(&T, bool)]` — that
    /// would mean borrowing all N slots (and allocating) every cycle, which on
    /// a wide merge costs more than the `n - 1` chained nodes it replaces.
    /// Instead it applies [`MergeN::winner`] — the shared rule the op's own
    /// `cycle` uses — to the engine's tick flags and clones only the winning
    /// slot.
    ///
    /// `srcs` must be non-empty; a zero-input merge has no value to produce
    /// (the fluent `merge_all(&[])` short-circuits to the receiver instead of
    /// wiring a node).
    pub fn merge_n<T: Clone + Default + 'static>(&mut self, srcs: &[Handle<T>]) -> Handle<T> {
        assert!(
            !srcs.is_empty(),
            "invariant: Builder::merge_n requires at least one upstream"
        );
        let idx = self.nodes.len();
        let indices: Vec<usize> = srcs.iter().map(|h| h.idx).collect();
        let slots: Vec<SlotRef<T>> = srcs.iter().map(|h| self.slot(*h)).collect();
        let out = self.new_slot(T::default());
        let out_reset = out.clone();
        let ticked = self.ticked.clone();
        self.push_node(
            indices.clone(),
            MergeN::<T>::ACTIVATION,
            "merge_n",
            Box::new(move |_k| {
                let winner = {
                    let t = ticked.borrow();
                    MergeN::<T>::winner(indices.iter().map(|&i| t[i]))
                };
                match winner {
                    Some(i) => {
                        let value = slots[i].borrow().clone();
                        *out.borrow_mut() = value;
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = T::default();
        }));
        self.make_handle(idx)
    }

    /// Pair each value with the current engine time: `(time, value)`. Kept
    /// hand-written (not `#[op]`): the output `(NanoTime, T)` is seeded from
    /// the input's current value, so it never requires `T: Default`.
    pub fn with_time<T: Clone + 'static>(&mut self, src: Handle<T>) -> Handle<(NanoTime, T)> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot((NanoTime::ZERO, src_slot.borrow().clone()));
        let (src_reset, out_reset) = (src_slot.clone(), out.clone());
        self.push_node(
            vec![src.idx],
            WithTime::<T>::ACTIVATION,
            "with_time",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match WithTime::<T>::cycle(&mut (), &mut (), (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = (NanoTime::ZERO, src_reset.borrow().clone());
        }));
        self.make_handle(idx)
    }

    /// The legacy `trimap`: combine three streams, each independently active
    /// or passive. All three values are read; only active inputs trigger.
    #[allow(clippy::too_many_arguments)]
    pub fn trimap<A, B, C, D, F>(
        &mut self,
        a: Handle<A>,
        a_active: bool,
        b: Handle<B>,
        b_active: bool,
        c: Handle<C>,
        c_active: bool,
        f: F,
    ) -> Handle<D>
    where
        A: 'static,
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&A, &B, &C) -> D + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let c_slot = self.slot(c);
        let out = self.new_slot(D::default());
        let cs = Self::cell(f, ());
        let out_reset = out.clone();
        let mut active = Vec::with_capacity(3);
        let mut passive = Vec::with_capacity(3);
        if a_active {
            active.push(a.idx);
        } else {
            passive.push(a.idx);
        }
        if b_active {
            active.push(b.idx);
        } else {
            passive.push(b.idx);
        }
        if c_active {
            active.push(c.idx);
        } else {
            passive.push(c.idx);
        }
        self.push_node(
            active,
            Join3::<A, B, C, D, F>::ACTIVATION,
            "trimap",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                let vc = c_slot.borrow();
                match Join3::<A, B, C, D, F>::cycle(cfg, state, (&va, &vb, &vc), &mut ctx)? {
                    Tick::Value(v) => {
                        drop((va, vb, vc));
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop((va, vb, vc));
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = D::default();
        }));
        self.set_passive_ups(idx, passive);
        self.make_handle(idx)
    }

    /// The legacy `try_trimap`: [`trimap`](Self::trimap) with a *fallible*
    /// closure. Any `Err` propagates to abort the run with context; the
    /// active/passive edge and dispatch semantics are identical to `trimap`.
    #[allow(clippy::too_many_arguments)]
    pub fn try_trimap<A, B, C, D, F>(
        &mut self,
        a: Handle<A>,
        a_active: bool,
        b: Handle<B>,
        b_active: bool,
        c: Handle<C>,
        c_active: bool,
        f: F,
    ) -> Handle<D>
    where
        A: 'static,
        B: 'static,
        C: 'static,
        D: Clone + Default + 'static,
        F: Fn(&A, &B, &C) -> Result<D> + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let c_slot = self.slot(c);
        let out = self.new_slot(D::default());
        let cs = Self::cell(f, ());
        let out_reset = out.clone();
        let mut active = Vec::with_capacity(3);
        let mut passive = Vec::with_capacity(3);
        if a_active {
            active.push(a.idx);
        } else {
            passive.push(a.idx);
        }
        if b_active {
            active.push(b.idx);
        } else {
            passive.push(b.idx);
        }
        if c_active {
            active.push(c.idx);
        } else {
            passive.push(c.idx);
        }
        self.push_node(
            active,
            TryJoin3::<A, B, C, D, F>::ACTIVATION,
            "try_trimap",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                let vc = c_slot.borrow();
                match TryJoin3::<A, B, C, D, F>::cycle(cfg, state, (&va, &vb, &vc), &mut ctx)? {
                    Tick::Value(v) => {
                        drop((va, vb, vc));
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop((va, vb, vc));
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = D::default();
        }));
        self.set_passive_ups(idx, passive);
        self.make_handle(idx)
    }

    /// The legacy `bimap`: combine two streams, each independently *active*
    /// (triggers the node when it ticks) or *passive* (read but not
    /// triggering). Both values are always read; only the active inputs
    /// appear in the dispatch condition. `join` is `bimap(_, true, _, true)`.
    pub fn bimap<A, B, C, F>(
        &mut self,
        a: Handle<A>,
        a_active: bool,
        b: Handle<B>,
        b_active: bool,
        f: F,
    ) -> Handle<C>
    where
        A: 'static,
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&A, &B) -> C + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let out = self.new_slot(C::default());
        let cs = Self::cell(f, ());
        let out_reset = out.clone();
        let mut active = Vec::with_capacity(2);
        let mut passive = Vec::with_capacity(2);
        if a_active {
            active.push(a.idx);
        } else {
            passive.push(a.idx);
        }
        if b_active {
            active.push(b.idx);
        } else {
            passive.push(b.idx);
        }
        self.push_node(
            active,
            Join::<A, B, C, F>::ACTIVATION,
            "bimap",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                match Join::<A, B, C, F>::cycle(cfg, state, (&va, &vb), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = C::default();
        }));
        self.set_passive_ups(idx, passive);
        self.make_handle(idx)
    }

    /// The legacy `try_bimap`: [`bimap`](Self::bimap) with a *fallible*
    /// closure. Any `Err` propagates to abort the run with context; the
    /// active/passive edge and dispatch semantics are identical to `bimap`.
    pub fn try_bimap<A, B, C, F>(
        &mut self,
        a: Handle<A>,
        a_active: bool,
        b: Handle<B>,
        b_active: bool,
        f: F,
    ) -> Handle<C>
    where
        A: 'static,
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&A, &B) -> Result<C> + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let out = self.new_slot(C::default());
        let cs = Self::cell(f, ());
        let out_reset = out.clone();
        let mut active = Vec::with_capacity(2);
        let mut passive = Vec::with_capacity(2);
        if a_active {
            active.push(a.idx);
        } else {
            passive.push(a.idx);
        }
        if b_active {
            active.push(b.idx);
        } else {
            passive.push(b.idx);
        }
        self.push_node(
            active,
            TryJoin::<A, B, C, F>::ACTIVATION,
            "try_bimap",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                match TryJoin::<A, B, C, F>::cycle(cfg, state, (&va, &vb), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = C::default();
        }));
        self.set_passive_ups(idx, passive);
        self.make_handle(idx)
    }

    /// A busy-poll source: `f` runs once per engine cycle, ticking on
    /// `Some`. Lossless and ordered (one value per cycle, no coalescing).
    /// The graph becomes a busy-spin loop in realtime mode — the kernel
    /// never parks. Realtime only.
    pub fn poll<T, F>(&mut self, f: F) -> Handle<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static,
    {
        let idx = self.nodes.len();
        let out = self.new_slot(T::default());
        let cs = Self::cell(f, ());
        self.has_always = true;
        // A busy-poll source is inherently single-run (realtime only).
        self.re_runnable = false;
        self.push_node(
            Vec::new(),
            Poll::<T, F>::ACTIVATION,
            "poll",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                match Poll::<T, F>::cycle(cfg, state, (), &mut ctx)? {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.make_handle(idx)
    }

    /// The legacy `latency_report`: a sink that observes each payload's
    /// embedded latency record into the caller-provided `stats`, and prints a
    /// summary at `stop` when `print_on_teardown`. Hand-written (not
    /// `register_op1`) because it carries a stop hook. The stats handle is
    /// passed in so the fluent [`LatencyReportOps`](crate::latency::LatencyReportOps)
    /// method can return it to the caller alongside the sink stream.
    pub fn latency_report<P>(
        &mut self,
        src: Handle<P>,
        print_on_teardown: bool,
        stats: Rc<RefCell<LatencyStats<P::L>>>,
    ) -> Handle<()>
    where
        P: Clone + Default + HasLatency + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(());
        let cs = Self::cell(
            LatencyReportCfg {
                stats,
                print_on_teardown,
            },
            (),
        );
        let cs_stop = cs.clone();
        let cs_reset = cs.clone();
        self.push_node(
            vec![src.idx],
            LatencyReport::<P>::ACTIVATION,
            "latency_report",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match LatencyReport::<P>::cycle(cfg, state, (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        // `latency_report`'s summary prints at stop, after the last cycle.
        let node = self
            .nodes
            .last_mut()
            .expect("invariant: latency_report node just pushed");
        node.stop = Box::new(move |k| {
            let (cfg, state) = &mut *cs_stop.borrow_mut();
            let mut ctx = Ctx::new(k, idx);
            LatencyReport::<P>::stop(cfg, state, &mut ctx)
        });
        // On a second `Runner::run`, start the aggregation fresh.
        self.set_reset(Box::new(move || {
            *cs_reset.borrow_mut().0.stats.borrow_mut() = LatencyStats::new();
        }));
        self.make_handle(idx)
    }

    /// Install a [`LatencyReportEach`] sink: the burst-shaped twin of
    /// [`latency_report`](Self::latency_report), observing every value in each
    /// burst rather than requiring the caller to `collapse()` first (which
    /// would discard all but the burst's last value, and every latency sample
    /// those values carried).
    pub fn latency_report_each<P>(
        &mut self,
        src: Handle<Burst<P>>,
        print_on_teardown: bool,
        stats: Rc<RefCell<LatencyStats<P::L>>>,
    ) -> Handle<()>
    where
        P: Clone + Default + HasLatency + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(());
        let cs = Self::cell(
            LatencyReportCfg {
                stats,
                print_on_teardown,
            },
            (),
        );
        let cs_stop = cs.clone();
        let cs_reset = cs.clone();
        self.push_node(
            vec![src.idx],
            LatencyReportEach::<P>::ACTIVATION,
            "latency_report_each",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match LatencyReportEach::<P>::cycle(cfg, state, (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        // As with `latency_report`, the summary prints at stop.
        let node = self
            .nodes
            .last_mut()
            .expect("invariant: latency_report_each node just pushed");
        node.stop = Box::new(move |k| {
            let (cfg, state) = &mut *cs_stop.borrow_mut();
            let mut ctx = Ctx::new(k, idx);
            LatencyReportEach::<P>::stop(cfg, state, &mut ctx)
        });
        self.set_reset(Box::new(move || {
            *cs_reset.borrow_mut().0.stats.borrow_mut() = LatencyStats::new();
        }));
        self.make_handle(idx)
    }

    /// Open a feedback edge: returns a source stream (no upstreams, so the
    /// graph stays acyclic) plus the [`FeedbackSink`] that feeds it. Values
    /// sent through the sink are emitted by the source on the *next* cycle.
    /// The source reads a shared time-queue and ticks when the sink has
    /// scheduled it — `Activation::SCHEDULES` for the callback-driven dispatch,
    /// though it is the sink (not the op) that does the scheduling.
    pub fn feedback<T>(&mut self) -> (Handle<T>, FeedbackSink<T>)
    where
        T: Clone + Default + PartialEq + 'static,
    {
        let idx = self.nodes.len();
        let out = self.new_slot(T::default());
        let queue: Rc<RefCell<TimeQueue<T>>> = Rc::new(RefCell::new(TimeQueue::new()));
        let q = queue.clone();
        let (q_reset, out_reset) = (queue.clone(), out.clone());
        self.push_node(
            Vec::new(),
            Activation::SCHEDULES,
            "feedback",
            Box::new(move |k| {
                let now = k.time();
                let mut ticked = false;
                while let Some(v) = q.borrow_mut().pop_if_pending(now) {
                    *out.borrow_mut() = v;
                    ticked = true;
                }
                Ok(ticked)
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *q_reset.borrow_mut() = TimeQueue::new();
            *out_reset.borrow_mut() = T::default();
        }));
        (self.make_handle(idx), FeedbackSink { queue, source: idx })
    }

    /// Wire the write end of a feedback edge: a pass-through of `src` that
    /// also pushes each value onto `sink`'s queue at `time + 1` and schedules
    /// the paired source node to emit it then. Returns the pass-through
    /// stream (identical values to `src`).
    pub fn feedback_send<T>(&mut self, src: Handle<T>, sink: &FeedbackSink<T>) -> Handle<T>
    where
        T: Clone + Default + PartialEq + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(T::default());
        let queue = sink.queue.clone();
        let source = sink.source;
        let out_reset = out.clone();
        self.push_node(
            vec![src.idx],
            Activation::NONE,
            "feedback_send",
            Box::new(move |k| {
                let at = k.time() + 1;
                let v = src_slot.borrow().clone();
                queue.borrow_mut().push(v.clone(), at);
                k.schedule(source, at);
                *out.borrow_mut() = v;
                Ok(true)
            }),
            Box::new(|_| Ok(())),
        );
        self.set_reset(Box::new(move || {
            *out_reset.borrow_mut() = T::default();
        }));
        self.make_handle(idx)
    }

    /// Mount a *composite* node: an entire compiled sub-graph behaving as a
    /// single node of this graph (the `nitro!` macro's `nested` expansion).
    ///
    /// The closure owns the sub-graph's state and is called once with
    /// `is_start = true` before the first cycle (to run inner `start` hooks
    /// and forward the earliest inner schedule), then once per activation.
    /// It reads its inputs through slot references captured at wiring time,
    /// so the engine only needs the active upstream indices for dispatch.
    /// This is the one dyn boundary the whole sub-graph pays per cycle.
    pub fn composite<T, F>(
        &mut self,
        active_ups: Vec<usize>,
        passive_ups: Vec<usize>,
        callback_activated: bool,
        always: bool,
        node: F,
    ) -> Handle<T>
    where
        T: Clone + Default + 'static,
        F: FnMut(&mut Ctx, CompositePhase) -> Result<Tick<T>> + 'static,
    {
        let idx = self.nodes.len();
        let out = self.new_slot(T::default());
        // A mounted island owns a private interior runtime with no reset hook,
        // so a graph containing one is single-run for now (see the re-run note
        // in the port plan).
        self.re_runnable = false;
        let cell = Rc::new(RefCell::new(node));
        let cell_start = cell.clone();
        let cell_stop = cell.clone();
        let cell_teardown = cell.clone();
        // An island holding a busy-poll (`Activation::ALWAYS`) op must say so
        // outward: the composite is one node to the outer engine, so unless it
        // declares `always` the outer dispatch never cycles it (a source island
        // has no upstream edge to be activated by) and the outer kernel parks
        // instead of spinning. Mirrors `Builder::poll` setting the same flag.
        if always {
            self.has_always = true;
        }
        let caps = Activation {
            schedules: callback_activated,
            threaded: false,
            always,
        };
        self.push_node(
            active_ups,
            caps,
            "graph",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                match (cell.borrow_mut())(&mut ctx, CompositePhase::Cycle)? {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                (cell_start.borrow_mut())(&mut ctx, CompositePhase::Start)?;
                Ok(())
            }),
        );
        // The island's inner `stop`/`teardown` hooks run when the *outer*
        // engine drives the composite node's own stop/teardown at cleanup —
        // so an island containing `print`/`timed`/`finally` flushes like any
        // other node. The closure itself keeps cleanup error-safe internally.
        let composite_node = self
            .nodes
            .last_mut()
            .expect("invariant: composite node just pushed");
        composite_node.stop = Box::new(move |k| {
            let mut ctx = Ctx::new(k, idx);
            (cell_stop.borrow_mut())(&mut ctx, CompositePhase::Stop)?;
            Ok(())
        });
        composite_node.teardown = Box::new(move |k| {
            let mut ctx = Ctx::new(k, idx);
            (cell_teardown.borrow_mut())(&mut ctx, CompositePhase::Teardown)?;
            Ok(())
        });
        // An island can read outer streams passively (a `nitro!` input marked
        // `passive`); those edges do not trigger the composite but must still
        // raise its layer above the values it reads.
        self.set_passive_ups(idx, passive_ups);
        self.make_handle(idx)
    }

    /// Register a **custom node** — the public, general-purpose extension point
    /// for a caller-driven graph node, the wingfoil equivalent of legacy
    /// `MutableNode` + `StreamPeekRef`. Where [`register_op1`](Self::register_op1)
    /// / [`register_op2`](Self::register_op2) fix the arity (1 or 2 active
    /// inputs) and own the op's state, `custom_node` declares an *arbitrary* set
    /// of active and passive upstream edges (by node index, so upstreams of any
    /// value type mix freely — only the indices matter for dispatch) and lets
    /// the `cycle` closure own whatever state it needs.
    ///
    /// The closure produces a [`Tick<T>`] each activation; the engine writes it
    /// into this node's output slot exactly as the op registrations do
    /// (`Value` ⇒ tick, `Silent` ⇒ update the value slot without ticking,
    /// `Quiet` ⇒ nothing). To *read* an upstream's current value, capture its
    /// [`SlotRef`] at wiring time (via
    /// [`Stream::value_slot`](crate::fluent::Stream::value_slot)) and
    /// `borrow()` it inside the closure — the engine never delivers inputs
    /// through [`Ctx`], which stays narrowed to time + self-scheduling. This
    /// mirrors legacy, where a `MutableNode` reaches its upstreams through the
    /// `Rc<dyn Stream>`s it holds.
    ///
    /// `activation` declares scheduling behaviour ([`Activation::NONE`] for a
    /// node driven purely by upstream ticks, [`Activation::SCHEDULES`] for one
    /// that self-schedules via [`Ctx::schedule`], etc.).
    ///
    /// **Single-run (v1):** a custom node's state is caller-owned with no engine
    /// `reset` hook, so — like a mounted island — a graph containing one is
    /// single-run; a second [`Runner::run`] errors rather than silently
    /// continuing stale state.
    pub fn custom_node<T, F>(
        &mut self,
        active_ups: Vec<usize>,
        passive_ups: Vec<usize>,
        activation: Activation,
        mut cycle: F,
    ) -> Handle<T>
    where
        T: Clone + Default + 'static,
        F: FnMut(&mut Ctx<'_>) -> Result<Tick<T>> + 'static,
    {
        let idx = self.nodes.len();
        let out = self.new_slot(T::default());
        // Caller-owned state has no engine reset hook, so a graph with a custom
        // node is single-run — the same contract as a mounted island (see the
        // re-run note in the port plan).
        self.re_runnable = false;
        // Honour the `always` activation the same way [`poll`](Self::poll) does:
        // a busy-poll custom node must keep the realtime kernel from parking
        // between cycles, else its `cycle` only fires on unrelated wakeups (a
        // socket-polling `ALWAYS` node would never advance). `build` already puts it
        // in `always_nodes` from its activation; this flips the engine into the
        // busy-spin loop so it is actually driven every cycle.
        if activation.always {
            self.has_always = true;
        }
        self.push_node(
            active_ups,
            activation,
            "custom",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                match cycle(&mut ctx)? {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Silent(v) => {
                        *out.borrow_mut() = v;
                        Ok(false)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        self.set_passive_ups(idx, passive_ups);
        self.make_handle(idx)
    }

    pub(crate) fn ticked_rc(&self) -> Rc<RefCell<Vec<bool>>> {
        self.ticked.clone()
    }

    /// The wired topology so far, as data — see
    /// [`introspect`](crate::introspect).
    ///
    /// Takes no clock reading, runs no op and reads no value slot, so it may
    /// be called at any point during wiring. Cheap enough for a test
    /// assertion; still a wiring-time facility, not something to call inside a
    /// cycle.
    ///
    /// The fluent equivalent is
    /// [`GraphBuilder::snapshot`](crate::fluent::GraphBuilder::snapshot); after
    /// [`build`](Self::build), use [`Runner::snapshot`].
    pub fn snapshot(&self) -> GraphSnapshot {
        GraphSnapshot::new(
            self.nodes
                .iter()
                .enumerate()
                .map(|(index, node)| node_info(index, node, false))
                .collect(),
        )
    }

    // Wingfoil's counterpart of legacy's `Graph::initialise` — the one-shot pass
    // that turns wiring into the dispatch topology. Named `initialise` in the
    // span so a subscriber (or a dashboard) reads the same across both engines.
    #[cfg_attr(
        feature = "instrument-initialise",
        tracing::instrument(skip_all, name = "initialise")
    )]
    pub fn build(self) -> Runner {
        // Sparse-dispatch topology, precomputed once so per-cycle work is
        // proportional to the nodes that fire, not the graph size `N`:
        //
        //   * `active_downs[u]` — the reverse of `active_ups`: the nodes a
        //     ticking `u` marks dirty. Dispatch propagates the tick frontier
        //     forward through these edges (passive edges are absent — read but
        //     not triggering).
        //   * `always_nodes` / `is_seed` — the frontier dispatch may seed each
        //     cycle: `always` (busy-poll) nodes, which fire unconditionally and
        //     so are kept as a list, and callback-activated ones (tickers,
        //     feedback source, channel/external, `delay`'s scheduled pop, …),
        //     which fire only when the kernel marks them and so are kept as a
        //     membership bitmap tested against `Kernel::due`. Seeding is
        //     therefore O(#fired), not O(#sources) and not O(N).
        //
        // Dispatch orders the per-cycle work set by ascending node **index**
        // (see `Runner::run`). Wiring order is a valid topological order over
        // *all* edges — active and passive — since the fluent API forces a
        // stream to exist before it is referenced, so index order processes
        // every node after everything it reads. That is what lets passive
        // reads (e.g. `sample` of a `delay`ed slot) observe the same value the
        // old full-index sweep produced, without tracking passive edges here.
        //
        // The work set drains by ascending **`(layer, index)`** (see
        // `Runner::run`), where `layer[i]` is the longest path to `i` over
        // *all* upstream edges — active and passive. This is legacy wingfoil's
        // layer-order dispatch (`dirty_nodes_by_layer`, `graph.rs:205`); for a
        // statically wired graph index order is already a valid layer order, so
        // `(layer, index)` is a linear extension of the read relation identical
        // to the old pure-index order (the `Dispatch::FullSweep` oracle pins
        // this). The explicit `layer` is what lets *dynamic* additions splice a
        // new node beneath an existing lower-indexed caller: `fix_layers` bumps
        // the caller's layer above the new node so it still drains after it —
        // the reorder index order alone cannot express.
        let n = self.nodes.len();
        let mut active_downs: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut passive_downs: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut always_nodes: Vec<usize> = Vec::new();
        let mut is_seed: Vec<bool> = vec![false; n];
        let mut layer: Vec<usize> = vec![0; n];
        for i in 0..n {
            let mut lyr = 0usize;
            for &u in &self.nodes[i].active_ups {
                active_downs[u].push(i);
                lyr = lyr.max(layer[u] + 1);
            }
            // Passive edges do not propagate ticks (absent from `active_downs`)
            // but still raise the layer: a passive reader must drain after the
            // value it reads, exactly as legacy counts every upstream
            // (`graph.rs:794`). Index order is topological over all edges, so
            // `layer[u]` is already final here. `passive_downs` is the reverse,
            // used only by a dynamic `fix_layers`.
            for &u in &self.nodes[i].passive_ups {
                passive_downs[u].push(i);
                lyr = lyr.max(layer[u] + 1);
            }
            layer[i] = lyr;
            let act = self.nodes[i].activation;
            if act.always {
                always_nodes.push(i);
            }
            is_seed[i] = act.always || act.callback_activated();
        }
        Runner {
            nodes: self.nodes,
            slots: self.slots,
            ticked: self.ticked,
            ready: self.ready,
            has_external: self.has_external,
            has_always: self.has_always,
            has_channel: self.has_channel,
            finished: self.finished,
            id: self.id,
            active_downs,
            passive_downs,
            removed: vec![false; n],
            pending: self.pending,
            marks: self.marks,
            has_marks: self.has_marks,
            always_nodes,
            is_seed,
            layer,
            dispatch: Dispatch::default(),
            timer: TimerPolicy::default(),
            re_runnable: self.re_runnable,
            has_run: false,
            node_visits: 0,
            layer_visits: 0,
            async_rt: self.async_rt,
        }
    }
}

/// Lowest layer `>= from` (and `<= max`) whose bucket holds work, per the
/// `occupied` bitmask — one bit per layer, set when a node is enqueued into that
/// layer's bucket and cleared when the bucket is drained.
///
/// This is what keeps the drain off an `O(depth)` walk of empty buckets: one
/// word test clears 64 layers at a time, so a deep-but-quiet graph costs
/// `depth/64` rather than `depth` per cycle. `from` may exceed `max` (the drain
/// has passed the deepest active layer), which reads as "nothing left".
/// `steps` counts each word examined, so callers can account for the scan's real
/// cost — see [`Runner::layer_visits`]. It is what makes the depth gate able to
/// tell a bitmask scan from a walk of every bucket: both open the same buckets,
/// but the walk examines 64x more to find them.
fn next_occupied_layer(
    occupied: &[u64],
    from: usize,
    max: usize,
    steps: &mut u64,
) -> Option<usize> {
    if from > max {
        return None;
    }
    let last_word = max >> 6;
    let mut w = from >> 6;
    // Mask off the bits below `from` in the first word — those layers are behind
    // the drain. `from & 63` is in `0..64`, so the shift is always in range.
    let mut word = occupied[w] & (!0u64 << (from & 63));
    loop {
        *steps += 1;
        if word != 0 {
            return Some((w << 6) | word.trailing_zeros() as usize);
        }
        w += 1;
        if w > last_word {
            return None;
        }
        word = occupied[w];
    }
}

/// Which dispatch strategy [`Runner::run`] uses. Both produce **identical**
/// observable results; they differ only in per-cycle cost.
///
/// [`Sparse`](Dispatch::Sparse) — the default and production path — drives a
/// dirty-list seeded from the tick frontier, so per-cycle work is proportional
/// to the nodes that actually fire. [`FullSweep`](Dispatch::FullSweep) is the
/// original `O(N)`-per-cycle topological sweep, retained as an executable
/// reference oracle: `runner.with_dispatch(Dispatch::FullSweep)` re-runs the
/// same graph under the old engine for differential parity checks and
/// sparse-vs-`N` benchmarking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dispatch {
    /// Sparse dirty-list (default): work ∝ active nodes.
    #[default]
    Sparse,
    /// Full topological sweep: work ∝ graph size `N`. Reference oracle.
    FullSweep,
}

/// Executes a wired graph. Dispatch is a sparse dirty-list — legacy wingfoil's
/// `dirty_nodes_by_layer` model — so per-cycle work is proportional to the
/// nodes that actually fire, not the graph size `N`. Each cycle seeds a work
/// set from the frontier ([`always_nodes`](Runner::always_nodes) busy-poll ops
/// plus the kernel's [`due`](Kernel::due) list), then propagates the
/// tick frontier forward: a node that ticks marks its active downstream
/// neighbours ([`active_downs`](Runner::active_downs)) dirty. The work set is
/// drained in ascending **`(layer, index)`** order ([`layer`](Runner::layer) =
/// longest path over active *and* passive edges), so each node fires exactly
/// once after everything it reads — glitch-free, single-fire, and
/// byte-identical to the previous full-index sweep, which index order alone
/// already satisfied on a static graph.
pub struct Runner {
    nodes: Vec<NodeRt>,
    slots: Vec<Rc<dyn Any>>,
    ticked: Rc<RefCell<Vec<bool>>>,
    ready: Option<ReadyReceiver>,
    has_external: bool,
    has_always: bool,
    has_channel: bool,
    finished: Rc<Cell<bool>>,
    id: u64,
    /// `active_downs[i]` = nodes triggered when `i` ticks (reverse of
    /// `active_ups`). Passive edges are deliberately absent — they are read but
    /// do not propagate ticks.
    active_downs: Vec<Vec<usize>>,
    /// `passive_downs[i]` = nodes that *passively* read `i` (reverse of
    /// `passive_ups`). Never used for tick propagation — only so a dynamic
    /// `fix_layers` can raise a passive reader's layer when the node it reads is
    /// relayered (legacy propagates through every downstream, active or
    /// passive). Inert unless the graph is mutated at runtime.
    #[cfg_attr(not(feature = "dynamic-graph"), allow(dead_code))]
    passive_downs: Vec<Vec<usize>>,
    /// `removed[i]` — a tombstone set when a node is deleted at runtime
    /// (`Extension::remove`). Its edges are unlinked and its `is_seed` flag is
    /// cleared so it never cycles again; the flag additionally stops the
    /// end-of-run cleanup from calling its `stop`/`teardown` a second time
    /// (they already ran at removal — legacy parity, `graph.rs:1015-1028`).
    /// Slots are tombstoned, never freed, so a `Handle` stays valid. Always
    /// all-false on a static run.
    #[cfg_attr(not(feature = "dynamic-graph"), allow(dead_code))]
    removed: Vec<bool>,
    /// Mutations staged by in-graph dynamic nodes during a cycle, drained and
    /// applied at each cycle boundary by [`run_dynamic`](Runner::run_dynamic).
    /// Shares the `Rc` the [`Builder`] handed to any staging node. Empty on a
    /// static graph.
    #[cfg_attr(not(feature = "dynamic-graph"), allow(dead_code))]
    pending: Rc<RefCell<Vec<PendingMut>>>,
    /// Same-cycle mark-dirty requests from routing nodes ([`demux`](Builder::demux)),
    /// drained by the dispatch loop within the current cycle. See [`Builder::marks`].
    marks: Rc<RefCell<Vec<usize>>>,
    /// Whether any routing node exists; gates the `marks` drain off the hot path.
    has_marks: bool,
    /// Busy-poll (`always`) ops, seeded unconditionally every cycle. Empty for
    /// every historical graph and for most realtime ones — an `always` op is a
    /// socket/ring-buffer poller.
    ///
    /// Its callback-activated counterpart is **not** a list to walk: those ops
    /// fire only when the kernel marks them, and the kernel already knows which
    /// ones it marked ([`Kernel::due`]), so dispatch reads the frontier from
    /// there and checks membership through [`is_seed`](Self::is_seed). Walking
    /// a list of every callback-activated node instead made a cycle cost
    /// `O(timers in the graph)`: 1024 idle tickers took the per-cycle cost of an
    /// 8-node hot path from 247 ns to 2.99 µs, none of it work the graph asked
    /// for.
    always_nodes: Vec<usize>,
    /// `is_seed[i]` — whether `i` may be seeded from the kernel's frontier
    /// (`always` or callback-activated). A dynamically removed node's flag is
    /// cleared, so a stale schedule left in the kernel cannot resurrect it.
    is_seed: Vec<bool>,
    /// `layer[i]` = longest path to `i` over all upstream edges (active and
    /// passive); the primary sparse-dispatch sort key. `(layer, index)` is a
    /// valid topological order that survives dynamic edge splices (which index
    /// order alone cannot — see [`build`](Builder::build) and `fix_layers`).
    layer: Vec<usize>,
    /// Which dispatch loop `run` uses. `Sparse` by default; see [`Dispatch`].
    dispatch: Dispatch,
    /// How a realtime run waits out the gap to the next scheduled callback.
    /// [`TimerPolicy::Park`] by default; see [`Runner::with_timer_policy`].
    timer: TimerPolicy,
    /// Whether every node can restore itself for a re-run (see
    /// [`Builder::re_runnable`](Builder)). A graph with `external`/`poll`/
    /// `channel` sources or `composite` islands is single-run.
    re_runnable: bool,
    /// Set after the first [`run`](Runner::run); a subsequent run first calls
    /// [`reset`](Runner::reset) to restore state.
    has_run: bool,
    /// Cumulative count of *node visits* — the observability hook behind the
    /// sparse-dispatch perf gate (see [`node_visits`](Runner::node_visits)).
    /// Accumulated once per cycle (`O(1)`, not per node), so it costs nothing
    /// on the hot path.
    node_visits: u64,
    /// Cumulative count of *layer visits* — buckets the drain actually opened.
    /// The depth-term counterpart of `node_visits`; see
    /// [`layer_visits`](Runner::layer_visits).
    layer_visits: u64,
    /// The graph-owned async runtime (or caller override), moved here from the
    /// [`Builder`] at [`build`](Builder::build). **Declared last on purpose:**
    /// Rust drops fields top-to-bottom, so `nodes` — and any offloaded sink whose
    /// teardown `Drop` does a `block_on` on this runtime's handle — is dropped
    /// *before* the runtime, keeping the runtime alive through teardown. Inert
    /// unless the `async` feature is on. Held only for its `Drop` (which stops
    /// producer tasks by dropping the owned runtime), never read.
    #[allow(dead_code)]
    async_rt: AsyncRuntimeSlot,
}

impl Runner {
    /// The built topology, as data — see [`introspect`](crate::introspect).
    ///
    /// The post-`build` counterpart of [`Builder::snapshot`], and the one to
    /// use from the fluent API, since
    /// [`GraphBuilder::build`](crate::fluent::GraphBuilder::build) consumes the
    /// builder. Identical for a static graph; on a graph mutated at runtime
    /// (the `dynamic-graph` feature) it additionally reflects spliced-in nodes
    /// and flags removed ones as
    /// [`NodeInfo::removed`](crate::introspect::NodeInfo::removed).
    ///
    /// Valid before, between and after runs. It reads no clock and no value
    /// slot, so calling it between runs cannot perturb a measurement.
    pub fn snapshot(&self) -> GraphSnapshot {
        GraphSnapshot::new(
            self.nodes
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    node_info(
                        index,
                        node,
                        self.removed.get(index).copied().unwrap_or(false),
                    )
                })
                .collect(),
        )
    }

    /// Run the graph to its bound. Returns the first error from any node's
    /// `start`/`cycle`/`stop`/`teardown` (with node context), or `Ok(())`.
    ///
    /// A [`Runner`] may be run **repeatedly** as long as its graph is
    /// re-runnable — tickers/constants + combinators + feedback, the
    /// deterministic historical subset. A second `run` first restores every
    /// node's state and value slot to its wiring-time initial value (via
    /// [`reset`](Runner::reset)), so each run is independent and reproduces a
    /// fresh graph exactly (spike 0.4's "setup-per-run" semantics). A graph
    /// with `external`/`poll`/`channel` sources or `composite` islands is
    /// single-run — its producer channels, wakers, and island interiors are
    /// consumed by the first run — and a second `run` returns an error rather
    /// than silently producing wrong values.
    #[cfg_attr(feature = "instrument-run", tracing::instrument(skip_all))]
    pub fn run(&mut self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        if self.has_run {
            if self.re_runnable {
                self.reset();
            } else {
                bail!(
                    "this Runner is single-run: its graph contains external/poll/channel \
                     sources or a nested island whose state cannot be reset — build a fresh \
                     graph to run again (a historical graph of tickers/constants + combinators \
                     re-runs)"
                );
            }
        }
        self.has_run = true;
        self.node_visits = 0;
        self.layer_visits = 0;
        let realtime = matches!(run_mode, RunMode::RealTime);
        // `external`/`poll` are wall-clock (realtime-only); `channel` carries
        // timestamps and runs in both modes. These are reachable user errors
        // (a caller choosing the wrong `RunMode`), so per CLAUDE.md's
        // error-handling rules they `bail!` rather than `assert!`.
        if !realtime && self.has_external {
            bail!(
                "graphs with external sources require RunMode::RealTime — untimestamped \
                 external events have no place in a deterministic historical replay (use a \
                 channel with timestamped sends for historical)"
            );
        }
        if !realtime && self.has_always {
            bail!(
                "graphs with poll sources require RunMode::RealTime — there is nothing to \
                 busy-poll in a deterministic historical replay"
            );
        }
        // The waker/ready channel is only used by realtime sources
        // (external, poll, realtime channel). A historical channel is
        // schedule-driven and needs no waker.
        let needs_waker = self.has_external || (self.has_channel && realtime);
        let mut kernel = if needs_waker {
            // The ready receiver is consumed by the first realtime run; a
            // second run of a graph with realtime sources is a reachable user
            // error, not an invariant, so `bail!` rather than `expect`.
            let Some(ready) = self.ready.take() else {
                bail!(
                    "a Runner with realtime sources (external/poll/realtime channel) supports \
                     only a single run — the waker/ready channel is consumed by the first run"
                );
            };
            Kernel::with_ready(run_mode, run_for, ready)
        } else {
            Kernel::new(run_mode, run_for)
        };
        if self.has_always {
            kernel.set_spin(true);
        }
        kernel.set_timer_policy(self.timer);
        // First error (from start or a cycle) wins; `stop`/`teardown` still
        // run afterwards regardless, matching the legacy engine.
        let mut first_err: Option<anyhow::Error> = None;

        {
            apply_nodes_span!("start");
            for (i, node) in self.nodes.iter_mut().enumerate() {
                if let Err(e) = (node.start)(&mut kernel) {
                    first_err = Some(e.context(format!("node {i} ({}) start", node.label)));
                    break;
                }
            }
        }

        if first_err.is_none() {
            // Both dispatch strategies produce identical observable results
            // (see `Dispatch`); the sparse dirty-list is the default, the full
            // sweep is retained as an executable reference oracle.
            first_err = match self.dispatch {
                Dispatch::Sparse => self.run_cycles_sparse(&mut kernel),
                Dispatch::FullSweep => self.run_cycles_full_sweep(&mut kernel),
            };
        }

        // Cleanup always runs; a stop/teardown error only surfaces if no
        // earlier error already won.
        {
            apply_nodes_span!("stop");
            for (i, node) in self.nodes.iter_mut().enumerate() {
                if let Err(e) = (node.stop)(&mut kernel) {
                    let e = e.context(format!("node {i} ({}) stop", node.label));
                    first_err.get_or_insert(e);
                }
            }
        }
        {
            apply_nodes_span!("teardown");
            for (i, node) in self.nodes.iter_mut().enumerate() {
                if let Err(e) = (node.teardown)(&mut kernel) {
                    let e = e.context(format!("node {i} ({}) teardown", node.label));
                    first_err.get_or_insert(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Restore every node's engine-owned state and value slot to its
    /// wiring-time initial value, so the next [`run`](Runner::run) starts from
    /// a clean slate — spike 0.4's per-run reset semantics. Called
    /// automatically by `run` on a re-run; exposed so a caller can reset
    /// explicitly. The kernel is rebuilt from `t=0` on each `run` and each
    /// node's `start` re-seeds its schedules, so this only touches per-node
    /// state (accumulators, buffers, delay queues), never the clock.
    ///
    /// Only meaningful for a re-runnable graph; on a single-run graph
    /// (external/poll/channel/island) the per-node reset closures still run but
    /// cannot restore a consumed producer channel, so [`run`](Runner::run)
    /// still refuses a second call.
    pub fn reset(&mut self) {
        for node in &mut self.nodes {
            (node.reset)();
        }
        // Defensive: every run leaves `ticked` all-false (each cycle clears the
        // nodes it fired), but clear it anyway so reset is self-contained.
        for t in self.ticked.borrow_mut().iter_mut() {
            *t = false;
        }
        self.finished.set(false);
    }

    /// Select the dispatch strategy for subsequent [`run`](Runner::run)s.
    /// Defaults to [`Dispatch::Sparse`]; [`Dispatch::FullSweep`] is the
    /// reference oracle (identical results, O(N) per cycle). Consumes and
    /// returns `self` so it chains off [`build`](Builder::build):
    /// `let mut r = g.build().with_dispatch(Dispatch::FullSweep);`.
    pub fn with_dispatch(mut self, dispatch: Dispatch) -> Self {
        self.dispatch = dispatch;
        self
    }

    /// Select how a **realtime** run waits out the gap to the next scheduled
    /// callback. Defaults to [`TimerPolicy::Park`]; consumes and returns
    /// `self` so it chains off [`build`](Builder::build):
    ///
    /// ```no_run
    /// # use std::time::Duration;
    /// # use wingfoil::prelude::*;
    /// # use wingfoil::{RunFor, RunMode, TimerPolicy};
    /// # let g = GraphBuilder::new();
    /// # let _ = g.ticker(Duration::from_millis(1)).count();
    /// let mut runner = g
    ///     .build()
    ///     .with_timer_policy(TimerPolicy::SpinAhead(Duration::from_micros(200)));
    /// runner.run(RunMode::RealTime, RunFor::Forever)?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    ///
    /// [`SpinAhead`](TimerPolicy::SpinAhead) buys wake-up accuracy with a core:
    /// the run sleeps to within the guard window of the deadline and spins the
    /// rest, so a `ticker`-driven strategy fires when it was scheduled rather
    /// than whenever the OS got round to it. It changes nothing about a
    /// historical replay (which never waits) or a graph with a busy-poll source
    /// (which never parks).
    pub fn with_timer_policy(mut self, timer: TimerPolicy) -> Self {
        self.timer = timer;
        self
    }

    /// The sparse dirty-list dispatch loop. Seeds the tick frontier into an
    /// index-ordered min-heap and drains it, propagating ticks through
    /// [`active_downs`](Runner::active_downs); per-cycle work is proportional to
    /// the nodes that fire. Returns the first cycle error, or `None` if the run
    /// reached its bound cleanly.
    fn run_cycles_sparse(&mut self, kernel: &mut Kernel) -> Option<anyhow::Error> {
        let n = self.nodes.len();
        let mut dirty = vec![false; n];
        // Sparse dirty-list scratch, allocated once and reused every cycle:
        //   * `buckets` — the dirty work set as per-layer vectors (legacy's
        //     `dirty_nodes_by_layer`), indexed by `layer[i]` and drained in
        //     ascending layer order. Every active downstream has a strictly
        //     greater layer than its upstream, so a bucket never grows once its
        //     layer is reached and each node fires exactly once after everything
        //     it reads. Bucketing by `layer` (not raw index) keeps that invariant
        //     after a dynamic splice bumps a caller's layer above a newly added
        //     upstream. `layer[i] < n`, so `n` buckets suffice.
        //   * `node_dirty[i]` — guards a recombine node against being enqueued
        //     twice, so it fires exactly once per cycle.
        //   * `fired` — every node cycled this tick; the per-cycle `ticked` and
        //     `node_dirty` resets touch only these, not all `N`, so per-cycle
        //     work stays proportional to the active node count.
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); n];
        // One bit per layer, marking which buckets hold work — see
        // `next_occupied_layer`. All-zero on entry and exit of every cycle.
        let mut occupied: Vec<u64> = vec![0; n.div_ceil(64).max(1)];
        let mut node_dirty = vec![false; n];
        let mut fired: Vec<usize> = Vec::new();
        // Check `finished` *before* `begin_cycle` parks: a channel that received
        // `EndOfStream` in the previous cycle ends the run now, rather than
        // waiting for the bound while a live sender clone keeps the waker
        // channel connected.
        while !self.finished.get() && kernel.begin_cycle(&mut dirty) {
            if let Some(e) = self.drain_cycle(
                kernel,
                &mut buckets,
                &mut occupied,
                &mut node_dirty,
                &mut fired,
            ) {
                return Some(e);
            }
            kernel.end_cycle(&mut dirty);
        }
        None
    }

    /// Seed, drain and reset a single already-begun cycle — the body shared by
    /// [`run_cycles_sparse`](Runner::run_cycles_sparse) and (behind
    /// `dynamic-graph`) [`run_dynamic`](Runner::run_dynamic). `dirty` was filled
    /// by `kernel.begin_cycle`; the scratch buffers are reused across cycles and
    /// must be empty (`fired`, and every `buckets` entry) / sized to the node
    /// count and all-false (`node_dirty`) on entry, and are restored to that
    /// state on a clean return. On a cycle error the buffers are left as-is (the
    /// run aborts, so they are not reused). Returns the first cycle error, or
    /// `None`.
    fn drain_cycle(
        &mut self,
        kernel: &mut Kernel,
        buckets: &mut [Vec<usize>],
        occupied: &mut [u64],
        node_dirty: &mut [bool],
        fired: &mut Vec<usize>,
    ) -> Option<anyhow::Error> {
        cycle_span!();
        // Seed the frontier into per-layer buckets: `always` ops fire
        // unconditionally; callback-activated ops (tickers, `delay` pops,
        // feedback source, channel replay) fire only when the kernel marked them
        // dirty this cycle — and the kernel hands those over directly, so this
        // costs the size of the frontier rather than a walk of every node that
        // *could* be on it. Everything else reaches a bucket by downstream
        // propagation below. `max_layer` tracks the deepest active layer so the
        // drain scans only `0..=max_layer`, not all `N` buckets.
        let mut max_layer = 0usize;
        // `is_seed` is the membership test the old walk got for free from its
        // seed list: it rejects a node the kernel marked but that dispatch must
        // not seed — a dynamically removed node whose stale schedule is still
        // queued.
        let frontier = self
            .always_nodes
            .iter()
            .copied()
            .chain(kernel.due().iter().copied().filter(|&i| self.is_seed[i]));
        for i in frontier {
            if !node_dirty[i] {
                node_dirty[i] = true;
                let l = self.layer[i];
                buckets[l].push(i);
                occupied[l >> 6] |= 1u64 << (l & 63);
                max_layer = max_layer.max(l);
            }
        }
        // Drain layer by layer in ascending order (legacy's
        // `dirty_nodes_by_layer`). A node that ticks marks its active downstream
        // neighbours — all at strictly higher layers, so their bucket has not
        // been reached yet — propagating the tick frontier. A bucket therefore
        // never grows once its own layer is processed, so draining it fully is
        // safe and each node fires exactly once after everything it reads.
        // Sorting each bucket by index reproduces legacy's within-layer wiring
        // order (byte-identical to the full-index sweep).
        //
        // The *occupied* layers are found through `occupied`, a bitmask with one
        // bit per layer, rather than by walking `0..=max_layer` and testing each
        // bucket. That walk made per-cycle cost `O(active + depth)`, which a deep
        // graph pays every cycle even when almost none of it fires — and back when
        // `fan` left-folded its branches into a merge chain, a wide fan-in *was*
        // deep (see Phase 4.5 in `docs/planning/port-plan.md`; `fan` now fans in
        // through one `merge_n`, but depth is still cheap to build by hand).
        // Scanning the mask skips 64 empty layers per word test, leaving
        // `O(active + depth/64)`.
        //
        // Deliberately a bitmask and not a heap of occupied layers: a heap would
        // be `O(active log active)` with a push/pop *per layer*, and on a linear
        // chain every layer holds one node — reintroducing the per-node heap
        // traffic whose removal is what closed the `fanout` gap against legacy.
        // The mask adds one branchless bit-set per enqueue and nothing per node.
        let mut scan_steps = 0u64;
        let mut l = 0usize;
        while let Some(next) = next_occupied_layer(occupied, l, max_layer, &mut scan_steps) {
            l = next;
            scan_steps += 1;
            // Clear the bit up front: pushes during this layer's drain all go to
            // strictly higher layers, so nothing can re-occupy `l` behind us.
            occupied[l >> 6] &= !(1u64 << (l & 63));
            // Move the layer's work out so downstream pushes (all to higher
            // layers) cannot alias it; the emptied allocation is returned below
            // for reuse next cycle.
            let mut current = std::mem::take(&mut buckets[l]);
            current.sort_unstable();
            for &i in &current {
                // Braced so the per-node span (when enabled) closes on the
                // node's own `cycle`, not on the downstream marking below.
                let did = {
                    cycle_node_span!(i, self.nodes[i].label);
                    match (self.nodes[i].cycle)(kernel) {
                        Ok(did) => did,
                        Err(e) => {
                            let label = self.nodes[i].label;
                            return Some(e.context(format!("node {i} ({label}) cycle")));
                        }
                    }
                };
                // `ticked[i]` must be visible to downstreams that read it
                // (`merge` tie-break, `delay` first-value seeding) before they
                // fire; layer order guarantees every node `i` reads has already
                // set its flag.
                self.ticked.borrow_mut()[i] = did;
                fired.push(i);
                if did {
                    for &d in &self.active_downs[i] {
                        if !node_dirty[d] {
                            node_dirty[d] = true;
                            let dl = self.layer[d];
                            buckets[dl].push(d);
                            occupied[dl >> 6] |= 1u64 << (dl & 63);
                            max_layer = max_layer.max(dl);
                        }
                    }
                }
                // Same-cycle routing: a `demux` parent marks a chosen,
                // already-wired child (strictly higher layer, so its bucket is
                // still ahead in the drain) to fire this cycle — even though the
                // parent itself did not tick.
                if self.has_marks {
                    let mut marks = self.marks.borrow_mut();
                    for target in marks.drain(..) {
                        if !node_dirty[target] {
                            node_dirty[target] = true;
                            let tl = self.layer[target];
                            buckets[tl].push(target);
                            occupied[tl >> 6] |= 1u64 << (tl & 63);
                            max_layer = max_layer.max(tl);
                        }
                    }
                }
            }
            current.clear();
            buckets[l] = current;
            l += 1;
        }
        // Per-cycle visit accounting for the perf gate: the seed scan plus the
        // nodes actually drained. Both terms are the sparse engine's real
        // per-cycle cost drivers — neither mentions `N`, which is exactly the
        // property `sparse_work_is_independent_of_graph_size` pins.
        self.node_visits += (self.always_nodes.len() + kernel.due().len() + fired.len()) as u64;
        self.layer_visits += scan_steps;
        // Reset only the nodes we touched (all buckets are empty again),
        // keeping the per-cycle reset sparse.
        {
            let mut t = self.ticked.borrow_mut();
            for &i in &*fired {
                t[i] = false;
                node_dirty[i] = false;
            }
        }
        fired.clear();
        None
    }

    /// The original full topological sweep, retained as a reference oracle:
    /// every cycle it walks **all** nodes in wiring order and runs those whose
    /// active upstream ticked (or which the kernel marked dirty) — `O(N)` per
    /// cycle regardless of how many nodes fire. Observably identical to
    /// [`run_cycles_sparse`](Runner::run_cycles_sparse); kept for differential
    /// testing and benchmarking (see [`Dispatch`]).
    fn run_cycles_full_sweep(&mut self, kernel: &mut Kernel) -> Option<anyhow::Error> {
        let n = self.nodes.len();
        let mut dirty = vec![false; n];
        // Same-cycle mark-dirty targets, the oracle's counterpart of the sparse
        // drain's `marks` handling. A routing node ([`Builder::demux`]) fires its
        // chosen child through this buffer *only* — the child has no active
        // upstream and no activation of its own — so a sweep that never drained
        // it silently dropped every demuxed tick and grew `marks` without bound
        // for the whole run. Wiring order puts a child after its parent, so
        // absorbing the marks as each node is visited fires it in the same cycle,
        // exactly as the sparse drain does.
        let mut marked = vec![false; n];
        while !self.finished.get() && kernel.begin_cycle(&mut dirty) {
            // Braced so the cycle span covers exactly what the sparse path's
            // `drain_cycle` does — the sweep and its per-cycle reset, not the
            // kernel's `end_cycle`.
            {
                cycle_span!();
                for (i, node) in self.nodes.iter_mut().enumerate() {
                    let due = node.activation.always
                        || (node.activation.callback_activated() && dirty[i])
                        || marked[i]
                        || {
                            let t = self.ticked.borrow();
                            node.active_ups.iter().any(|&u| t[u])
                        };
                    let did = if due {
                        cycle_node_span!(i, node.label);
                        match (node.cycle)(kernel) {
                            Ok(did) => did,
                            Err(e) => {
                                return Some(e.context(format!("node {i} ({}) cycle", node.label)));
                            }
                        }
                    } else {
                        false
                    };
                    self.ticked.borrow_mut()[i] = did;
                    if self.has_marks {
                        for target in self.marks.borrow_mut().drain(..) {
                            if let Some(m) = marked.get_mut(target) {
                                *m = true;
                            }
                        }
                    }
                }
                // The oracle's whole cost model: every node is visited every
                // cycle, whether or not it is due. This is the `O(N)` term the
                // sparse loop above does not pay, and what makes `node_visits` a
                // *sensitive* metric rather than one that reads the same for
                // both strategies.
                self.node_visits += n as u64;
                for t in self.ticked.borrow_mut().iter_mut() {
                    *t = false;
                }
                for m in marked.iter_mut() {
                    *m = false;
                }
            }
            kernel.end_cycle(&mut dirty);
        }
        None
    }

    /// Node visits performed by the most recent [`run`](Runner::run) — the
    /// deterministic metric behind the Phase 4.5 sparse-dispatch gate.
    ///
    /// A *visit* is one node the dispatch loop had to look at, whether or not it
    /// fired. Under [`Dispatch::Sparse`] that is the per-cycle seed scan plus
    /// the nodes actually drained; under [`Dispatch::FullSweep`] it is `N` per
    /// cycle, since the oracle tests every node's due-ness. The difference is
    /// the point: the sparse engine's count is a function of how much of the
    /// graph is *active*, so padding a graph with quiet nodes must not move it,
    /// while the oracle's count scales with the padding.
    ///
    /// Wall-clock benchmarks (`benches/store_baseline.rs`) measure the same
    /// property with timing noise attached; this counter is exact, so a test can
    /// assert on it and fail a regression to an all-nodes sweep outright. It is
    /// accumulated once per cycle, not per node, so it is free on the hot path.
    ///
    /// Reset at the start of each run, so it always describes the latest one
    /// (`run_dynamic` included — there the graph size itself moves, so the
    /// count is only meaningful against a known mutation sequence).
    ///
    /// Hidden: an engine-observability hook for the perf gates, not public API.
    #[doc(hidden)]
    pub fn node_visits(&self) -> u64 {
        self.node_visits
    }

    /// How many nodes the wired graph holds — the *static* counterpart of
    /// [`node_visits`](Runner::node_visits)'s per-run count.
    ///
    /// Exists so a test can pin the cost of a wiring shape rather than of a
    /// run. The gate it was added for: an n-way `merge_all` / `fan` must cost
    /// **one** merge node, not the `n - 1` a binary chain costs. That
    /// distinction is invisible to a results-parity test (a chain and an n-ary
    /// node produce identical values) and was invisible to the benchmarks too,
    /// which is how a 1.86x loss against legacy on a wide fan-in survived —
    /// see `MergeN`.
    ///
    /// Hidden: an engine-observability hook for the perf gates, not public API.
    #[doc(hidden)]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Layer-scan steps performed by the most recent [`run`](Runner::run) — the
    /// depth-term counterpart of [`node_visits`](Runner::node_visits).
    ///
    /// Counts the work the drain does to *find and open* its layers: one per
    /// bucket opened, plus one per word examined while looking for the next
    /// occupied layer. The second term is the point. Opening buckets is
    /// unavoidable and identical under any strategy; what distinguishes them is
    /// how much is examined to find those buckets. Walking `0..=max_layer` tests
    /// every layer, so quiet depth costs `depth` per cycle; scanning the
    /// occupied-layer bitmask clears 64 layers per word, so it costs `depth/64`.
    /// A gate can tell those apart only if the examining is counted.
    ///
    /// Reset at the start of each run, like `node_visits`.
    ///
    /// Hidden: an engine-observability hook for the perf gates, not public API.
    #[doc(hidden)]
    pub fn layer_visits(&self) -> u64 {
        self.layer_visits
    }

    /// Current value of a node's output slot.
    pub fn value<T: Clone + 'static>(&self, h: impl AsHandle<T>) -> T {
        let h = h.as_handle();
        debug_assert_eq!(
            h.builder_id, self.id,
            "Handle used with a different Runner than the Builder that minted it"
        );
        self.slots[h.idx]
            .clone()
            .downcast::<RefCell<T>>()
            .expect("invariant: Handle<T> indexes a slot of type T")
            .borrow()
            .clone()
    }
}

// ---- Runtime graph dynamism (feature `dynamic-graph`) ----------------------
//
// Runtime add/splice on the live interpreted graph. Built on the always-on
// layered `(layer, index)` dispatch: because dispatch order is a `layer` key
// (not raw node index), a new node can be appended at the end of the `nodes`
// vec (highest index) and *spliced beneath* an existing lower-indexed caller —
// `fix_layers` lifts the caller's layer above the new node so it still drains
// after it, the reorder legacy wingfoil does with its own `layer`/`fix_layers`
// (`graph.rs:1149`) and index order alone cannot express.
//
// `run_dynamic` mirrors `run` but hands a mutation scope to a caller-supplied
// hook at each cycle boundary — the exact point legacy applies its staged
// `pending_additions`/`pending_removals` (`graph.rs:934-939`), so a node added
// after cycle N first fires in cycle N+1. This is the driver-thread surface;
// the in-`cycle` node-driven path (what `DynamicGroup` needs) stages into the
// same boundary apply and lands in a later increment.

#[cfg(feature = "dynamic-graph")]
impl Runner {
    /// Longest-path dispatch layer of a node — the first component of the
    /// `(layer, index)` sort key. Exposed for dynamism tests that assert
    /// `fix_layers` re-sorted a spliced node correctly.
    #[doc(hidden)]
    pub fn layer_of<T>(&self, h: impl AsHandle<T>) -> usize {
        self.layer[h.as_handle().index()]
    }

    /// Run the graph like [`run`](Runner::run), but call `between` at every
    /// cycle boundary with a mutation scope ([`Extension`]) over the live graph
    /// and the number of cycles completed so far. Nodes appended (or edges
    /// spliced) through the scope take effect on the *next* cycle — legacy
    /// wingfoil's "requested in cycle N, live in N+1" contract.
    #[cfg_attr(feature = "instrument-run", tracing::instrument(skip_all))]
    pub fn run_dynamic<F>(
        &mut self,
        run_mode: RunMode,
        run_for: RunFor,
        mut between: F,
    ) -> Result<()>
    where
        F: FnMut(&mut Extension<'_>, u64) -> Result<()>,
    {
        // Source/run-mode validation, identical to `run`.
        self.node_visits = 0;
        self.layer_visits = 0;
        let realtime = matches!(run_mode, RunMode::RealTime);
        if !realtime && self.has_external {
            bail!(
                "graphs with external sources require RunMode::RealTime — untimestamped \
                 external events have no place in a deterministic historical replay (use a \
                 channel with timestamped sends for historical)"
            );
        }
        if !realtime && self.has_always {
            bail!(
                "graphs with poll sources require RunMode::RealTime — there is nothing to \
                 busy-poll in a deterministic historical replay"
            );
        }
        let needs_waker = self.has_external || (self.has_channel && realtime);
        let mut kernel = if needs_waker {
            let Some(ready) = self.ready.take() else {
                bail!(
                    "a Runner with realtime sources (external/poll/realtime channel) supports \
                     only a single run — the waker/ready channel is consumed by the first run"
                );
            };
            Kernel::with_ready(run_mode, run_for, ready)
        } else {
            Kernel::new(run_mode, run_for)
        };
        if self.has_always {
            kernel.set_spin(true);
        }
        kernel.set_timer_policy(self.timer);

        let mut first_err: Option<anyhow::Error> = None;
        {
            apply_nodes_span!("start");
            for (i, node) in self.nodes.iter_mut().enumerate() {
                if let Err(e) = (node.start)(&mut kernel) {
                    first_err = Some(e.context(format!("node {i} ({}) start", node.label)));
                    break;
                }
            }
        }

        if first_err.is_none() {
            // Scratch is reused across cycles and regrown as nodes are appended.
            // `dirty` is sized to the current node count each cycle (the kernel
            // marks due callbacks into it by index).
            let mut buckets: Vec<Vec<usize>> = Vec::new();
            let mut occupied: Vec<u64> = Vec::new();
            let mut node_dirty: Vec<bool> = Vec::new();
            let mut fired: Vec<usize> = Vec::new();
            // The kernel's dirty flags. Grown (never reallocated per cycle) as
            // nodes are appended, and left all-false by every `end_cycle`, so a
            // long dynamic run does not allocate and zero a graph-sized array on
            // every one of its cycles.
            let mut dirty: Vec<bool> = Vec::new();
            // `u64`, not `u32`: this is handed to the caller's `between` hook on
            // every cycle, and a realtime dynamic graph outlives a `u32`
            // (`Kernel::cycles` carries the same widening for the same reason).
            let mut cycles: u64 = 0;
            loop {
                let n = self.nodes.len();
                if node_dirty.len() < n {
                    node_dirty.resize(n, false);
                }
                if dirty.len() < n {
                    dirty.resize(n, false);
                }
                // `layer[i] < n`, so `n` buckets always suffice; regrow as nodes
                // are appended (splices only add layers at the top).
                if buckets.len() < n {
                    buckets.resize_with(n, Vec::new);
                }
                // Same for the occupied-layer mask: dynamic splices only add
                // layers at the top, so growing it preserves the set bits.
                if occupied.len() < n.div_ceil(64) {
                    occupied.resize(n.div_ceil(64), 0);
                }
                if self.finished.get() || !kernel.begin_cycle(&mut dirty) {
                    break;
                }
                if let Some(e) = self.drain_cycle(
                    &mut kernel,
                    &mut buckets,
                    &mut occupied,
                    &mut node_dirty,
                    &mut fired,
                ) {
                    first_err = Some(e);
                    break;
                }
                kernel.end_cycle(&mut dirty);
                cycles += 1;
                // Apply mutations staged by in-graph dynamic nodes during this
                // cycle (e.g. a `dynamic_group`'s insert/remove), matching
                // legacy's end-of-cycle `process_pending_*` (`graph.rs:934-939`).
                let staged = std::mem::take(&mut *self.pending.borrow_mut());
                for apply in staged {
                    if let Err(e) = apply(self, &mut kernel) {
                        first_err = Some(e);
                        break;
                    }
                }
                if first_err.is_some() {
                    break;
                }
                // Then the caller-driven mutation scope (driver-thread surface).
                let mut ext = Extension {
                    runner: self,
                    kernel: &mut kernel,
                    appended: Vec::new(),
                };
                if let Err(e) = between(&mut ext, cycles) {
                    first_err = Some(e);
                    break;
                }
            }
        }

        // Cleanup always runs; a stop/teardown error only surfaces if no
        // earlier error already won. A node removed mid-run already ran its
        // `stop`/`teardown` at removal (legacy parity), so the tombstone skips
        // it here — no double call.
        {
            apply_nodes_span!("stop");
            for i in 0..self.nodes.len() {
                if self.removed[i] {
                    continue;
                }
                if let Err(e) = (self.nodes[i].stop)(&mut kernel) {
                    let e = e.context(format!("node {i} ({}) stop", self.nodes[i].label));
                    first_err.get_or_insert(e);
                }
            }
        }
        {
            apply_nodes_span!("teardown");
            for i in 0..self.nodes.len() {
                if self.removed[i] {
                    continue;
                }
                if let Err(e) = (self.nodes[i].teardown)(&mut kernel) {
                    let e = e.context(format!("node {i} ({}) teardown", self.nodes[i].label));
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // Dynamic node registration reaches the value store through the same
    // `SlotRef` boundary as static wiring (`Builder::slot`/`new_slot`), so a
    // future arena/SoA swap need not special-case dynamically-added slots.
    fn rt_slot<T: 'static>(&self, h: Handle<T>) -> SlotRef<T> {
        debug_assert_eq!(
            h.builder_id, self.id,
            "Handle used with a different Runner than the Builder that minted it"
        );
        SlotRef::new(
            self.slots[h.idx]
                .clone()
                .downcast::<RefCell<T>>()
                .expect("invariant: Handle<T> indexes a slot of type T"),
        )
    }

    fn rt_new_slot<T: 'static>(&mut self, init: T) -> SlotRef<T> {
        let cell = Rc::new(RefCell::new(init));
        self.slots.push(cell.clone() as Rc<dyn Any>);
        SlotRef::new(cell)
    }

    fn rt_make_handle<T>(&self, idx: usize) -> Handle<T> {
        Handle {
            idx,
            builder_id: self.id,
            _t: PhantomData,
        }
    }

    /// Append a node to the *live* graph, growing every parallel structure the
    /// sparse engine keeps (`nodes`, `ticked`, `active_downs`, `passive_downs`,
    /// `layer`, `is_seed`, and `always_nodes` if it busy-polls) and wiring its
    /// reverse edges + layer. `active_ups`/`passive_ups` reference existing
    /// (lower-index) nodes, so no existing node's order changes — pure append.
    fn rt_append_node(
        &mut self,
        active_ups: Vec<usize>,
        passive_ups: Vec<usize>,
        activation: Activation,
        label: &'static str,
        cycle: CycleFn,
        start: LifecycleFn,
    ) -> usize {
        let idx = self.nodes.len();
        let mut lyr = 0usize;
        for &u in &active_ups {
            self.active_downs[u].push(idx);
            lyr = lyr.max(self.layer[u] + 1);
        }
        for &u in &passive_ups {
            self.passive_downs[u].push(idx);
            lyr = lyr.max(self.layer[u] + 1);
        }
        self.nodes.push(NodeRt {
            active_ups,
            passive_ups,
            activation,
            label,
            cycle,
            start,
            stop: Box::new(|_| Ok(())),
            teardown: Box::new(|_| Ok(())),
            // Dynamically-added nodes default to a no-op reset — re-run
            // (a second `run`) is a static-graph capability; a graph mutated
            // via `run_dynamic` rebuilds its dynamic region on the next run.
            reset: Box::new(|| {}),
        });
        self.ticked.borrow_mut().push(false);
        self.active_downs.push(Vec::new());
        self.passive_downs.push(Vec::new());
        self.removed.push(false);
        self.layer.push(lyr);
        if activation.always {
            self.always_nodes.push(idx);
        }
        self.is_seed
            .push(activation.always || activation.callback_activated());
        idx
    }

    /// Splice an edge from existing `new` into existing `caller`, then
    /// re-establish layer order. An active edge (`active`) records
    /// `caller`→`active_ups` + the reverse `active_downs[new]` so `new`'s tick
    /// re-fires `caller`; a passive edge records `passive_ups`/`passive_downs`
    /// only. `caller` may now depend on a *higher-indexed* `new`, so
    /// `fix_layers` lifts its layer above `new` — the reorder only the
    /// `(layer, index)` key can dispatch.
    fn splice_upstream(&mut self, caller: usize, new: usize, active: bool) {
        if active {
            self.nodes[caller].active_ups.push(new);
            self.active_downs[new].push(caller);
        } else {
            self.nodes[caller].passive_ups.push(new);
            self.passive_downs[new].push(caller);
        }
        self.fix_layers(caller);
    }

    /// Recompute `start`'s layer from its upstreams (active *and* passive) and
    /// propagate any increase to every node that reads it (active *and*
    /// passive downstreams), iterative BFS. Port of legacy's `fix_layers`
    /// (`graph.rs:1149`): after splicing a new upstream into an existing
    /// caller, this lifts the caller — and anything reading it — above the new
    /// node so `(layer, index)` dispatch still drains each node after
    /// everything it reads.
    fn fix_layers(&mut self, start: usize) {
        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(start);
        while let Some(node) = queue.pop_front() {
            let required = self.nodes[node]
                .active_ups
                .iter()
                .chain(self.nodes[node].passive_ups.iter())
                .map(|&u| self.layer[u])
                .max()
                .map_or(0, |m| m + 1);
            if required > self.layer[node] {
                self.layer[node] = required;
                for &d in self.active_downs[node]
                    .iter()
                    .chain(self.passive_downs[node].iter())
                {
                    queue.push_back(d);
                }
            }
        }
    }

    /// Unlink node `idx` from the live graph and run its lifecycle teardown.
    /// Removes it from every upstream's down-list and every downstream's
    /// up-list (both active and passive), drops it from the dispatch frontier, runs
    /// `stop` then `teardown` once, and tombstones it (`removed[idx] = true`).
    /// Because it no longer appears in any frontier or reverse-edge list it can
    /// never be enqueued again. Port of legacy's `process_pending_removals`
    /// (`graph.rs:992-1028`). No-op if already removed.
    fn remove_node(&mut self, idx: usize, kernel: &mut Kernel) -> Result<()> {
        if self.removed[idx] {
            return Ok(());
        }
        // Unlink from upstreams' down-lists.
        let active_ups = std::mem::take(&mut self.nodes[idx].active_ups);
        for &u in &active_ups {
            self.active_downs[u].retain(|&x| x != idx);
        }
        let passive_ups = std::mem::take(&mut self.nodes[idx].passive_ups);
        for &u in &passive_ups {
            self.passive_downs[u].retain(|&x| x != idx);
        }
        // Unlink from downstreams' up-lists.
        let active_downs = std::mem::take(&mut self.active_downs[idx]);
        for &d in &active_downs {
            self.nodes[d].active_ups.retain(|&x| x != idx);
        }
        let passive_downs = std::mem::take(&mut self.passive_downs[idx]);
        for &d in &passive_downs {
            self.nodes[d].passive_ups.retain(|&x| x != idx);
        }
        // Drop from the dispatch frontier so it is never seeded again — including
        // via a schedule the kernel is still holding for it.
        self.always_nodes.retain(|&x| x != idx);
        self.is_seed[idx] = false;
        self.removed[idx] = true;
        // Lifecycle teardown, once, with node context — stop then teardown.
        let label = self.nodes[idx].label;
        (self.nodes[idx].stop)(kernel)
            .map_err(|e| e.context(format!("node {idx} ({label}) stop (dynamic removal)")))?;
        (self.nodes[idx].teardown)(kernel)
            .map_err(|e| e.context(format!("node {idx} ({label}) teardown (dynamic removal)")))?;
        Ok(())
    }

    /// Schedule the attachment points of a freshly appended region to fire at
    /// `time + 1`, in dependency order — the `recycle` first-value guarantee.
    /// Walks `new`'s upstream cone, bounded to nodes in `appended` (this
    /// boundary's new nodes); a node is an *attachment point* if it reads a
    /// pre-existing (non-`appended`) upstream or is a source. Each attachment
    /// point is scheduled and marked in `is_seed` so the scheduled dirty flag
    /// actually fires it (a plain
    /// `map`/`fold` is otherwise reached only by upstream propagation).
    /// Downstream appended nodes then fire by normal tick propagation, so the
    /// region evaluates in order and reads real values, not `Default`. Mirrors
    /// legacy's attachment-point walk (`graph.rs:1092-1115`).
    fn recycle_schedule(&mut self, new: usize, appended: &[usize], kernel: &mut Kernel) {
        let time = kernel.time() + 1;
        let mut stack = vec![new];
        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
        while let Some(ix) = stack.pop() {
            if !visited.insert(ix) {
                continue;
            }
            let has_preexisting = self.nodes[ix]
                .active_ups
                .iter()
                .chain(self.nodes[ix].passive_ups.iter())
                .any(|u| !appended.contains(u));
            let is_source =
                self.nodes[ix].active_ups.is_empty() && self.nodes[ix].passive_ups.is_empty();
            if has_preexisting || is_source {
                kernel.schedule(ix, time);
                self.is_seed[ix] = true;
            }
            for &u in self.nodes[ix]
                .active_ups
                .iter()
                .chain(self.nodes[ix].passive_ups.iter())
            {
                if appended.contains(&u) {
                    stack.push(u);
                }
            }
        }
    }
}

/// A scoped mutation session over a *live* [`Runner`], handed to the
/// [`run_dynamic`](Runner::run_dynamic) boundary hook. Every node appended or
/// edge spliced here takes effect on the next cycle. Handles it mints carry the
/// runner's `builder_id`, so cross-runner misuse stays caught.
#[cfg(feature = "dynamic-graph")]
pub struct Extension<'r> {
    runner: &'r mut Runner,
    /// The live kernel, so `add_upstream(recycle = true)` can schedule the new
    /// region's attachment points to fire at `time + 1`.
    kernel: &'r mut Kernel,
    /// Indices appended through *this* boundary scope, so a subsequent
    /// `add_upstream(recycle = true)` knows which of `new`'s upstream cone is
    /// freshly added (walk into) vs. pre-existing (an attachment point).
    appended: Vec<usize>,
}

#[cfg(feature = "dynamic-graph")]
impl Extension<'_> {
    /// Append a `map` of an existing `src` onto the live graph. It ticks from
    /// the next cycle whenever `src` ticks; its value is observable via
    /// [`Runner::value`].
    pub fn map<A, B, F>(&mut self, src: impl AsHandle<A>, f: F) -> Handle<B>
    where
        A: 'static,
        B: Clone + Default + 'static,
        F: Fn(&A) -> B + 'static,
    {
        let src = src.as_handle();
        let idx = self.runner.nodes.len();
        let src_slot = self.runner.rt_slot(src);
        let out = self.runner.rt_new_slot(B::default());
        let cs = Rc::new(RefCell::new((f, ())));
        let cycle: CycleFn = Box::new(move |k| {
            let (cfg, state) = &mut *cs.borrow_mut();
            let mut ctx = Ctx::new(k, idx);
            let a = src_slot.borrow();
            match crate::ops::Map::<A, B, F>::cycle(cfg, state, (&a,), &mut ctx)? {
                Tick::Value(v) => {
                    drop(a);
                    *out.borrow_mut() = v;
                    Ok(true)
                }
                Tick::Silent(v) => {
                    drop(a);
                    *out.borrow_mut() = v;
                    Ok(false)
                }
                Tick::Quiet => Ok(false),
            }
        });
        self.runner.rt_append_node(
            vec![src.idx],
            Vec::new(),
            crate::ops::Map::<A, B, F>::ACTIVATION,
            "map",
            cycle,
            Box::new(|_| Ok(())),
        );
        self.appended.push(idx);
        self.runner.rt_make_handle(idx)
    }

    /// Append a `fold` of an existing `src` onto the live graph — a running
    /// accumulator seeded with `init`, updated each time `src` ticks.
    pub fn fold<A, B, F>(&mut self, src: impl AsHandle<A>, init: B, f: F) -> Handle<B>
    where
        A: 'static,
        B: Clone + 'static,
        F: Fn(&mut B, &A) + 'static,
    {
        let src = src.as_handle();
        let idx = self.runner.nodes.len();
        let src_slot = self.runner.rt_slot(src);
        let out = self.runner.rt_new_slot(init.clone());
        let cs = Rc::new(RefCell::new((f, init)));
        let cycle: CycleFn = Box::new(move |k| {
            let (cfg, state) = &mut *cs.borrow_mut();
            let mut ctx = Ctx::new(k, idx);
            let a = src_slot.borrow();
            match crate::ops::Fold::<A, B, F>::cycle(cfg, state, (&a,), &mut ctx)? {
                Tick::Value(v) => {
                    drop(a);
                    *out.borrow_mut() = v;
                    Ok(true)
                }
                Tick::Silent(v) => {
                    drop(a);
                    *out.borrow_mut() = v;
                    Ok(false)
                }
                Tick::Quiet => Ok(false),
            }
        });
        self.runner.rt_append_node(
            vec![src.idx],
            Vec::new(),
            crate::ops::Fold::<A, B, F>::ACTIVATION,
            "fold",
            cycle,
            Box::new(|_| Ok(())),
        );
        self.appended.push(idx);
        self.runner.rt_make_handle(idx)
    }

    /// Append a value-predicate filter of an existing `src` onto the live graph:
    /// it re-emits `src`'s value on the cycles `pred` holds and stays quiet
    /// otherwise — the per-key selector a `dynamic_group` factory typically
    /// builds over a shared feed.
    pub fn filter_value<A, F>(&mut self, src: impl AsHandle<A>, pred: F) -> Handle<A>
    where
        A: Clone + Default + 'static,
        F: Fn(&A) -> bool + 'static,
    {
        let src = src.as_handle();
        let idx = self.runner.nodes.len();
        let src_slot = self.runner.rt_slot(src);
        let out = self.runner.rt_new_slot(A::default());
        let cycle: CycleFn = Box::new(move |_k| {
            let v = src_slot.borrow();
            if pred(&v) {
                let val = v.clone();
                drop(v);
                *out.borrow_mut() = val;
                Ok(true)
            } else {
                Ok(false)
            }
        });
        self.runner.rt_append_node(
            vec![src.idx],
            Vec::new(),
            Activation::NONE,
            "filter_value",
            cycle,
            Box::new(|_| Ok(())),
        );
        self.appended.push(idx);
        self.runner.rt_make_handle(idx)
    }

    /// Splice `new` in as an upstream of the existing `caller`. An `active`
    /// edge re-fires `caller` whenever `new` ticks and lifts `caller`'s layer
    /// above `new` (via `fix_layers`) so dispatch order stays correct even
    /// though `caller` has the lower index; a passive edge is read-only but
    /// still raises the layer.
    ///
    /// With `recycle = true`, the newly appended region feeding `new` is
    /// scheduled to fire at `time + 1` in dependency order (its attachment
    /// points — nodes reading a pre-existing upstream, or new sources — are
    /// seeded), so `caller` observes real current values on the next cycle
    /// rather than the `Default` a not-yet-run node would hold. Mirrors legacy
    /// `add_upstream(recycle)` (`graph.rs:1092-1116`). Takes effect next cycle.
    pub fn add_upstream<C, N>(
        &mut self,
        caller: impl AsHandle<C>,
        new: impl AsHandle<N>,
        active: bool,
        recycle: bool,
    ) {
        let caller = caller.as_handle();
        let new = new.as_handle();
        self.runner.splice_upstream(caller.idx, new.idx, active);
        if recycle {
            self.runner
                .recycle_schedule(new.idx, &self.appended, self.kernel);
        }
    }

    /// Remove a node from the live graph: unlink its edges, drop it from the
    /// dispatch frontier so it never cycles again, and run its `stop` then
    /// `teardown` exactly once (now, not at run end). The slot is tombstoned
    /// (never freed), so the handle stays valid and its last value is still
    /// readable — legacy parity (`graph.rs:992-1028`). Idempotent: removing an
    /// already-removed node is a no-op.
    pub fn remove<T>(&mut self, node: impl AsHandle<T>) -> Result<()> {
        self.runner.remove_node(node.as_handle().idx, self.kernel)
    }
}

/// One live per-key stream a [`dynamic_group`](Builder::dynamic_group) tracks:
/// its output value slot and its node index (to read its per-cycle tick flag).
///
/// Public only so it can name the value type in a [`StreamStore`] bound on
/// [`dynamic_group_with_store`](Builder::dynamic_group_with_store); its fields
/// are engine internals and stay private (a store only ever moves whole
/// `LiveStream`s around, never inspects them).
#[cfg(feature = "dynamic-graph")]
pub struct LiveStream<T> {
    slot: SlotRef<T>,
    idx: usize,
}

/// Backing-store abstraction for [`dynamic_group_with_store`](Builder::dynamic_group_with_store)
/// — the wingfoil twin of legacy `StreamStore` (`nodes/dynamic_group.rs`). A
/// `dynamic_group` keeps its live per-key members in one of these; the trait
/// abstracts over the container so the key can be `Ord` ([`BTreeMap`]) *or*
/// merely `Hash + Eq` ([`HashMap`]). Blanket impls cover both; implement it for
/// any other keyed collection.
///
/// `BTreeMap` stays the default (via [`dynamic_group`](Builder::dynamic_group)):
/// its deterministic iteration order is the safer backtest default, and the
/// per-cycle cost is iterating *live* members regardless of the container.
#[cfg(feature = "dynamic-graph")]
pub trait StreamStore<K, V> {
    /// Insert (or replace) the member stored under `key`.
    fn store_insert(&mut self, key: K, value: V);
    /// Remove and return the member stored under `key`, if any.
    fn store_remove(&mut self, key: &K) -> Option<V>;
    /// Iterate `(key, member)` pairs in the store's native order.
    fn store_entries<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)>
    where
        K: 'a,
        V: 'a;
}

#[cfg(feature = "dynamic-graph")]
impl<K: Ord, V> StreamStore<K, V> for std::collections::BTreeMap<K, V> {
    fn store_insert(&mut self, key: K, value: V) {
        self.insert(key, value);
    }

    fn store_remove(&mut self, key: &K) -> Option<V> {
        self.remove(key)
    }

    fn store_entries<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)>
    where
        K: 'a,
        V: 'a,
    {
        self.iter()
    }
}

#[cfg(feature = "dynamic-graph")]
impl<K: std::hash::Hash + Eq, V> StreamStore<K, V> for std::collections::HashMap<K, V> {
    fn store_insert(&mut self, key: K, value: V) {
        self.insert(key, value);
    }

    fn store_remove(&mut self, key: &K) -> Option<V> {
        self.remove(key)
    }

    fn store_entries<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)>
    where
        K: 'a,
        V: 'a,
    {
        self.iter()
    }
}

#[cfg(feature = "dynamic-graph")]
impl Builder {
    /// A keyed collection of dynamically-wired sub-graphs, kept in sync with the
    /// graph — the wingfoil twin of legacy `dynamic_group_stream`
    /// (`nodes/dynamic_group.rs`). A single in-graph node reacts to two key
    /// streams and folds its live members into an output value `V`:
    ///
    /// - when `add` ticks, `factory` builds a per-key sub-graph (via an
    ///   [`Extension`]) whose output is wired in as an **active** upstream with
    ///   `recycle` (so it observes real current values); its handle is tracked
    ///   under the key;
    /// - when `del` ticks, `on_remove` runs and the key's output node is removed;
    /// - every cycle, `on_tick` folds each tracked member **that ticked this
    ///   cycle** into `V`. Because each member is an active upstream, the layered
    ///   engine drains the group *after* them, so those reads are the members'
    ///   current values — the fresh-read guarantee dynamic groups depend on.
    ///
    /// Runs under [`Runner::run_dynamic`] (its boundary is where the staged
    /// insert/remove mutations apply). The live members are held in a
    /// `BTreeMap` (so `K: Ord`); use
    /// [`dynamic_group_with_store`](Self::dynamic_group_with_store) to back them
    /// with a `HashMap` (or any [`StreamStore`]) for a `Hash + Eq` key that is
    /// not `Ord`.
    #[allow(clippy::too_many_arguments)]
    pub fn dynamic_group<K, T, V, Factory, OnTick, OnRemove>(
        &mut self,
        add: Handle<K>,
        del: Handle<K>,
        factory: Factory,
        init: V,
        on_tick: OnTick,
        on_remove: OnRemove,
    ) -> Handle<V>
    where
        K: Clone + Default + Ord + 'static,
        T: Clone + Default + 'static,
        V: Clone + Default + 'static,
        Factory: Fn(&mut Extension<'_>, K) -> Handle<T> + 'static,
        OnTick: Fn(&mut V, &K, &T) + 'static,
        OnRemove: Fn(&mut V, &K) + 'static,
    {
        self.dynamic_group_with_store(
            add,
            del,
            factory,
            std::collections::BTreeMap::new(),
            init,
            on_tick,
            on_remove,
        )
    }

    /// Like [`dynamic_group`](Self::dynamic_group) but with the live members
    /// held in a caller-supplied [`StreamStore`] — the wingfoil twin of legacy
    /// `dynamic_group_stream_with_store`. Pass a `HashMap::new()` to key the
    /// group by a `Hash + Eq` type that is not `Ord`:
    ///
    /// ```ignore
    /// b.dynamic_group_with_store(
    ///     add, del, factory,
    ///     std::collections::HashMap::new(), // Hash + Eq key, no Ord needed
    ///     init, on_tick, on_remove,
    /// );
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn dynamic_group_with_store<K, T, V, S, Factory, OnTick, OnRemove>(
        &mut self,
        add: Handle<K>,
        del: Handle<K>,
        factory: Factory,
        store: S,
        init: V,
        on_tick: OnTick,
        on_remove: OnRemove,
    ) -> Handle<V>
    where
        K: Clone + Default + 'static,
        T: Clone + Default + 'static,
        V: Clone + Default + 'static,
        S: StreamStore<K, LiveStream<T>> + 'static,
        Factory: Fn(&mut Extension<'_>, K) -> Handle<T> + 'static,
        OnTick: Fn(&mut V, &K, &T) + 'static,
        OnRemove: Fn(&mut V, &K) + 'static,
    {
        let idx = self.nodes.len();
        let add_slot = self.slot(add);
        let del_slot = self.slot(del);
        let out = self.new_slot(init.clone());
        let ticked = self.ticked.clone();
        let pending = self.pending.clone();
        let (add_idx, del_idx) = (add.idx, del.idx);

        let store: Rc<RefCell<S>> = Rc::new(RefCell::new(store));
        let factory = Rc::new(factory);
        let mut value = init;

        let cycle: CycleFn = Box::new(move |_k| {
            let (add_t, del_t) = {
                let t = ticked.borrow();
                (t[add_idx], t[del_idx])
            };
            // Add: stage the factory build + active/recycle splice for the
            // boundary (the store insert happens there too, once it has a slot).
            if add_t {
                let key: K = add_slot.borrow().clone();
                let f = factory.clone();
                let store_ins = store.clone();
                pending.borrow_mut().push(Box::new(
                    move |runner: &mut Runner, kernel: &mut Kernel| {
                        let mut ext = Extension {
                            runner,
                            kernel,
                            appended: Vec::new(),
                        };
                        let h = f(&mut ext, key.clone());
                        let slot = ext.runner.rt_slot(h);
                        let caller = ext.runner.rt_make_handle::<V>(idx);
                        ext.add_upstream(caller, h, true, true);
                        store_ins.borrow_mut().store_insert(
                            key,
                            LiveStream {
                                slot,
                                idx: h.index(),
                            },
                        );
                        Ok(())
                    },
                ));
            }
            // Delete: run `on_remove` and drop from the store now (so it stops
            // aggregating immediately); stage the node removal for the boundary.
            if del_t {
                let key: K = del_slot.borrow().clone();
                on_remove(&mut value, &key);
                let removed = store.borrow_mut().store_remove(&key);
                if let Some(live) = removed {
                    pending.borrow_mut().push(Box::new(
                        move |runner: &mut Runner, kernel: &mut Kernel| {
                            runner.remove_node(live.idx, kernel)
                        },
                    ));
                }
            }
            // Aggregate the members that ticked this cycle. `ticked[live.idx]`
            // and `live.slot` are the member's current tick/value: the member is
            // an active upstream, so it drained before this node (layer order).
            let mut ticked_any = false;
            {
                let t = ticked.borrow();
                let members = store.borrow();
                for (key, live) in members.store_entries() {
                    if t[live.idx] {
                        let v = live.slot.borrow().clone();
                        on_tick(&mut value, key, &v);
                        ticked_any = true;
                    }
                }
            }
            *out.borrow_mut() = value.clone();
            Ok(ticked_any)
        });

        self.push_node(
            vec![add_idx, del_idx],
            Activation::NONE,
            "dynamic_group",
            cycle,
            Box::new(|_| Ok(())),
        );
        self.make_handle(idx)
    }

    /// Fixed-topology dynamic *routing* — the wingfoil twin of legacy `demux`
    /// (`nodes/demux.rs`). Pre-wires `size` child streams plus one overflow
    /// child; each cycle the parent reads `source`, calls `route(value)` for a
    /// slot, and marks **only** the chosen child to fire this cycle (via the
    /// engine's same-cycle mark-dirty). A slot `< size` selects that child;
    /// anything `>= size` routes to the overflow child. The selected child
    /// re-emits the source's current value; the others stay quiet. No nodes are
    /// added or removed — the graph shape is fixed, only the tick is routed.
    ///
    /// Returns `(children, overflow)`. This is the raw routing primitive:
    /// key→slot assignment and slot release are the caller's concern. For
    /// legacy's auto-assigning / `Close`-releasing key lifecycle, use
    /// [`demux_map`](Self::demux_map) (single value) or
    /// [`demux_it`](Self::demux_it) (a `Burst` per child), which layer a
    /// [`DemuxMap`] on top of this primitive.
    pub fn demux<T, F>(
        &mut self,
        source: Handle<T>,
        size: usize,
        route: F,
    ) -> (Vec<Handle<T>>, Handle<T>)
    where
        T: Clone + Default + 'static,
        F: Fn(&T) -> usize + 'static,
    {
        self.has_marks = true;
        let source_slot = self.slot(source);

        // Parent: reads `source`, routes, and marks the chosen child. It never
        // ticks itself (`Ok(false)`) — it publishes the value for the child to
        // read and hands the tick to that one child via the mark-dirty buffer.
        let parent_idx = self.nodes.len();
        let parent_out = self.new_slot(T::default());
        let child_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let marks = self.marks.clone();
        let idxs_cycle = child_indices.clone();
        let publish = parent_out.clone();
        let cycle: CycleFn = Box::new(move |_k| {
            let value = source_slot.borrow().clone();
            *publish.borrow_mut() = value.clone();
            let slot = route(&value).min(size); // `>= size` → overflow (last entry)
            let target = idxs_cycle.borrow()[slot];
            marks.borrow_mut().push(target);
            Ok(false)
        });
        self.push_node(
            vec![source.idx],
            Activation::NONE,
            "demux",
            cycle,
            Box::new(|_| Ok(())),
        );

        // `size` children + 1 overflow. Each reads the parent's published value
        // passively (so its layer sits above the parent and it drains *after*
        // the mark) but is never triggered by it — only the mark fires it.
        let mut children = Vec::with_capacity(size);
        let mut overflow = None;
        for slot in 0..=size {
            let child_idx = self.nodes.len();
            let read = parent_out.clone();
            let out = self.new_slot(T::default());
            let cycle: CycleFn = Box::new(move |_k| {
                let v = read.borrow().clone();
                *out.borrow_mut() = v;
                Ok(true)
            });
            self.push_node(
                Vec::new(),
                Activation::NONE,
                "demux_child",
                cycle,
                Box::new(|_| Ok(())),
            );
            self.set_passive_ups(child_idx, vec![parent_idx]);
            child_indices.borrow_mut().push(child_idx);
            let handle = self.make_handle::<T>(child_idx);
            if slot < size {
                children.push(handle);
            } else {
                overflow = Some(handle);
            }
        }
        (children, overflow.expect("overflow child built"))
    }

    /// Auto-assigning demux — the wingfoil twin of legacy `demux`
    /// (`StreamOperators::demux`). Layers a [`DemuxMap`] key lifecycle over the
    /// raw [`demux`](Self::demux) primitive: each cycle `func(value)` yields a
    /// key and a [`DemuxEvent`]; the map hands each *new* key a free slot from a
    /// pool of `capacity`, and [`DemuxEvent::Close`] routes to the key's current
    /// slot and then releases it back to the pool. Keys arriving with no slot
    /// free route to the overflow child.
    ///
    /// Returns `(children, overflow)` exactly like [`demux`](Self::demux); the
    /// only added behaviour is the automatic slot bookkeeping.
    pub fn demux_map<T, K, F>(
        &mut self,
        source: Handle<T>,
        capacity: usize,
        func: F,
    ) -> (Vec<Handle<T>>, Handle<T>)
    where
        T: Clone + Default + 'static,
        K: std::hash::Hash + Eq + 'static,
        F: Fn(&T) -> (K, DemuxEvent) + 'static,
    {
        let map = DemuxMap::new(capacity);
        // `route` returns `capacity` (== size) for overflow, which the raw
        // `demux` primitive maps to the overflow child.
        let route = move |v: &T| {
            let (key, event) = func(v);
            let slot = match event {
                DemuxEvent::Close => map.release(&key),
                DemuxEvent::None => map.get_or_insert(key),
            };
            slot.unwrap_or(capacity)
        };
        self.demux(source, capacity, route)
    }

    /// Iterable demux — the wingfoil twin of legacy `demux_it`
    /// (`StreamOperators::demux_it`). Where [`demux_map`](Self::demux_map) routes
    /// one value to one child, this routes *each item* of an iterable source
    /// value to its keyed child, and every selected child re-emits a
    /// [`Burst`] of exactly the items routed to it this cycle. Unselected
    /// children stay quiet. Same [`DemuxMap`] key lifecycle as
    /// [`demux_map`](Self::demux_map).
    ///
    /// Built on the same same-cycle mark-dirty primitive as
    /// [`demux`](Self::demux) (no engine changes): the parent clears one
    /// `Burst` slot per child, accumulates each item into its slot's burst, and
    /// marks that child dirty; each child reads its own burst slice.
    pub fn demux_it<I, T, K, F>(
        &mut self,
        source: Handle<I>,
        capacity: usize,
        func: F,
    ) -> (Vec<Handle<Burst<T>>>, Handle<Burst<T>>)
    where
        I: IntoIterator<Item = T> + Clone + Default + 'static,
        T: Clone + Default + 'static,
        K: std::hash::Hash + Eq + 'static,
        F: Fn(&T) -> (K, DemuxEvent) + 'static,
    {
        self.has_marks = true;
        let source_slot = self.slot(source);
        let map = DemuxMap::new(capacity);

        // Parent: publishes one `Burst` per child (capacity children + 1
        // overflow), routes each source item into its slot's burst, and marks
        // that child. It never ticks itself (`Ok(false)`).
        let parent_idx = self.nodes.len();
        let parent_out = self.new_slot(vec![Burst::<T>::new(); capacity + 1]);
        let child_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let marks = self.marks.clone();
        let idxs_cycle = child_indices.clone();
        let publish = parent_out.clone();
        let cycle: CycleFn = Box::new(move |_k| {
            let items = source_slot.borrow().clone();
            let mut rows = publish.borrow_mut();
            for row in rows.iter_mut() {
                row.clear();
            }
            for item in items {
                let (key, event) = func(&item);
                let slot = match event {
                    DemuxEvent::Close => map.release(&key),
                    DemuxEvent::None => map.get_or_insert(key),
                }
                .unwrap_or(capacity); // overflow slot == capacity (last row)
                rows[slot].push(item);
                let target = idxs_cycle.borrow()[slot];
                marks.borrow_mut().push(target);
            }
            Ok(false)
        });
        self.push_node(
            vec![source.idx],
            Activation::NONE,
            "demux_it",
            cycle,
            Box::new(|_| Ok(())),
        );

        // `capacity` children + 1 overflow; each reads its own burst slice from
        // the parent's published vec (passive, so it drains after the mark) and
        // fires only when the parent marked it.
        let mut children = Vec::with_capacity(capacity);
        let mut overflow = None;
        for slot in 0..=capacity {
            let child_idx = self.nodes.len();
            let read = parent_out.clone();
            let out = self.new_slot(Burst::<T>::new());
            let cycle: CycleFn = Box::new(move |_k| {
                let v = read.borrow()[slot].clone();
                *out.borrow_mut() = v;
                Ok(true)
            });
            self.push_node(
                Vec::new(),
                Activation::NONE,
                "demux_it_child",
                cycle,
                Box::new(|_| Ok(())),
            );
            self.set_passive_ups(child_idx, vec![parent_idx]);
            child_indices.borrow_mut().push(child_idx);
            let handle = self.make_handle::<Burst<T>>(child_idx);
            if slot < capacity {
                children.push(handle);
            } else {
                overflow = Some(handle);
            }
        }
        (children, overflow.expect("overflow child built"))
    }
}

/// Signals, for a demuxed value, whether it opens/continues a key's slot or
/// releases it. The wingfoil twin of legacy `DemuxEvent` (`nodes/demux.rs`), used
/// by [`Builder::demux_map`] and [`Builder::demux_it`].
#[cfg(feature = "dynamic-graph")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxEvent {
    /// Route to the key's slot, assigning a free slot if the key is new.
    None,
    /// Route to the key's current slot, then release it back to the pool.
    Close,
}

/// Auto-assigning key→slot map backing [`Builder::demux_map`] /
/// [`Builder::demux_it`] — the wingfoil twin of legacy `DemuxMap`
/// (`nodes/demux.rs`). Hands each new key a free slot from a pool of `capacity`;
/// [`DemuxEvent::Close`] frees a key's slot again. A key that arrives with no
/// slot free is pinned to overflow until it is released.
#[cfg(feature = "dynamic-graph")]
struct DemuxMap<K: std::hash::Hash + Eq> {
    inner: RefCell<DemuxMapInner<K>>,
}

#[cfg(feature = "dynamic-graph")]
struct DemuxMapInner<K: std::hash::Hash + Eq> {
    /// Slots `0..capacity` not currently assigned to a key. A `BTreeSet` (not
    /// legacy's `HashSet`) so a new key always claims the *lowest* free slot —
    /// making slot assignment deterministic, the safer backtest default.
    available: std::collections::BTreeSet<usize>,
    /// Assigned keys → their slot (`None` means the key overflowed: no slot was
    /// free when it first arrived).
    in_use: std::collections::HashMap<K, Option<usize>>,
}

#[cfg(feature = "dynamic-graph")]
impl<K: std::hash::Hash + Eq> DemuxMap<K> {
    fn new(capacity: usize) -> Self {
        Self {
            inner: RefCell::new(DemuxMapInner {
                available: (0..capacity).collect(),
                in_use: std::collections::HashMap::new(),
            }),
        }
    }

    /// Return the key's slot (`Some(ix)`), assigning a free one if the key is
    /// new; `None` means overflow (already-overflowed key, or no slot free).
    fn get_or_insert(&self, key: K) -> Option<usize> {
        let mut m = self.inner.borrow_mut();
        if let Some(entry) = m.in_use.get(&key) {
            return *entry;
        }
        match m.available.iter().next().copied() {
            Some(ix) => {
                m.available.remove(&ix);
                m.in_use.insert(key, Some(ix));
                Some(ix)
            }
            None => {
                m.in_use.insert(key, None);
                None
            }
        }
    }

    /// Route the key's final value to its current slot, then free that slot back
    /// to the pool. `None` means overflow. Releasing an unknown key routes to any
    /// currently-free slot (legacy parity) without mutating the pool.
    fn release(&self, key: &K) -> Option<usize> {
        let mut m = self.inner.borrow_mut();
        match m.in_use.remove(key) {
            Some(Some(ix)) => {
                m.available.insert(ix);
                Some(ix)
            }
            Some(None) => None,
            None => m.available.iter().next().copied(),
        }
    }
}
