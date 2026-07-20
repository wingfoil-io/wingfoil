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
//! Execution model (and a known gap): each cycle this engine sweeps **all**
//! nodes in wiring (topological) order and runs those whose active upstream
//! ticked (or which the kernel marked dirty). Insertion order is always a
//! valid topological order — the fluent API forces a stream to exist before
//! it can be referenced — so this is glitch-free and fires each node exactly
//! once after its upstreams, giving results **identical** to classic
//! wingfoil. But classic instead propagates **breadth-first from the ticked
//! sources through a dirty-list**, touching only nodes that can fire; the
//! `O(N)`-per-cycle sweep here does not match that on large, sparsely-ticking
//! graphs. Closing that (breadth-first dirty-list + arena value store) is a
//! planned engine phase — see `docs/port-plan.md` "Phase 4.5"; it is a
//! mechanism/performance change only, results stay identical.
//!
//! Value slots are individual `Rc<RefCell<T>>`s (the arena is part of that
//! same phase). `run` is fallible — it returns the first
//! `start`/`cycle`/`stop`/`teardown` error (with node context) and still runs
//! cleanup afterwards, matching the classic engine.

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;

use crate::Burst;
use crate::channel::{ChannelSender, Message};
use crate::op::{Activation, Ctx, Op, Tick};
use crate::ops::{
    Const, Delay, DelayState, Filter, Finally, Fold, Join, Join3, Merge2, Poll, Sample, Throttle,
    Ticker, Window, WindowState, WithTime,
};
use wingfoil::codegen::{Kernel, KernelWaker, ReadyReceiver, waker_channel};
use wingfoil::{NanoTime, RunFor, RunMode, TimeQueue};

/// Anything that identifies a node's typed output — a raw [`Handle`] or a
/// fluent [`Stream`](crate::fluent::Stream).
pub trait AsHandle<T> {
    fn as_handle(&self) -> Handle<T>;
}

/// A typed reference to a node's output within a [`Builder`] / [`Runner`].
pub struct Handle<T> {
    idx: usize,
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

/// Trim a `type_name` — `wingfoil_next::ops::Map<u64, …, {{closure}}>` — down
/// to the bare op name (`Map`) for error context: drop everything from the
/// first `<`, then keep only the final `::` segment. A plain label with no
/// path or generics passes through unchanged, so hand-written and
/// `#[op]`-generated nodes read the same in error messages.
pub(crate) fn short_type_name(s: &'static str) -> &'static str {
    let head = s.split('<').next().unwrap_or(s);
    head.rsplit("::").next().unwrap_or(head)
}

type CycleFn = Box<dyn FnMut(&mut Kernel) -> Result<bool>>;
/// Start / stop / teardown all share this shape.
type LifecycleFn = Box<dyn FnMut(&mut Kernel) -> Result<()>>;

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

/// Wires a graph of [`Op`]s. Combinators mirror the classic fluent API but
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
        (
            Handle {
                idx,
                _t: PhantomData,
            },
            source,
        )
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
    ///   wall-clock arrival (the classic `produce_async` model). Same-time
    ///   values ride one atomic burst, never split or dropped.
    ///
    /// A `Message::Error` propagates into the graph and aborts the run.
    pub fn channel<T: Clone + Default + 'static>(
        &mut self,
    ) -> (Handle<Burst<T>>, ChannelSender<T>) {
        let idx = self.nodes.len();
        let out = self.new_slot(Burst::<T>::new());
        let (tx, rx) = std::sync::mpsc::channel::<Message<T>>();
        self.has_channel = true;
        // Shared between the cycle and start adapters: the receiver, plus the
        // time-grouped bursts the historical `start` fills.
        let cs = Self::cell(rx, VecDeque::<(NanoTime, Burst<T>)>::new());
        let cs2 = cs.clone();
        self.push_node(
            Vec::new(),
            Activation {
                schedules: true,
                threaded: true,
                always: false,
            },
            "channel",
            Box::new(move |k| {
                match k.run_mode() {
                    // Historical: emit the burst grouped at the current time.
                    RunMode::HistoricalFrom(_) => {
                        let now = k.time();
                        let (_, groups) = &mut *cs.borrow_mut();
                        match groups.front() {
                            Some((t, _)) if *t <= now => {
                                let (_, burst) = groups.pop_front().expect("front checked");
                                *out.borrow_mut() = burst;
                                Ok(true)
                            }
                            _ => Ok(false),
                        }
                    }
                    // Realtime: drain everything pending into one burst.
                    RunMode::RealTime => {
                        let (rx, _) = &mut *cs.borrow_mut();
                        let mut burst: Burst<T> = Burst::new();
                        loop {
                            match rx.try_recv() {
                                Ok(Message::Value(v) | Message::ValueAt(v, _)) => burst.push(v),
                                Ok(Message::Error(e)) => {
                                    return Err(anyhow::anyhow!("{e:#}")
                                        .context("channel receiver: producer sent an error"));
                                }
                                Ok(Message::EndOfStream | Message::Checkpoint(_)) => {}
                                Err(_) => break,
                            }
                        }
                        if burst.is_empty() {
                            Ok(false)
                        } else {
                            *out.borrow_mut() = burst;
                            Ok(true)
                        }
                    }
                }
            }),
            Box::new(move |k| {
                // Historical: block-collect the whole timestamped stream up
                // front (producer sends values then closes), group same-time
                // values into bursts, and schedule one delivery per timestamp.
                if let RunMode::HistoricalFrom(_) = k.run_mode() {
                    let start_time = k.start_time();
                    let mut collected: Vec<(NanoTime, T)> = Vec::new();
                    {
                        let (rx, _) = &mut *cs2.borrow_mut();
                        loop {
                            match rx.recv() {
                                Ok(Message::ValueAt(v, t)) => collected.push((t, v)),
                                Ok(Message::Value(v)) => collected.push((start_time, v)),
                                Ok(Message::Checkpoint(_)) => {}
                                Ok(Message::EndOfStream) => break,
                                Ok(Message::Error(e)) => {
                                    return Err(anyhow::anyhow!("{e:#}")
                                        .context("channel receiver: producer sent an error"));
                                }
                                // All senders dropped without an explicit close.
                                Err(_) => break,
                            }
                        }
                    }
                    // Stable sort by time keeps same-time values in send order,
                    // then group consecutive equal timestamps into one burst.
                    collected.sort_by_key(|(t, _)| *t);
                    let (_, groups) = &mut *cs2.borrow_mut();
                    for (t, v) in collected {
                        match groups.back_mut() {
                            Some((bt, burst)) if *bt == t => burst.push(v),
                            _ => groups.push_back((t, Burst::from([v]))),
                        }
                    }
                    for (t, _) in groups.iter() {
                        k.schedule(idx, *t);
                    }
                }
                Ok(())
            }),
        );
        let sender = ChannelSender::new(tx, self.waker.clone(), idx);
        (
            Handle {
                idx,
                _t: PhantomData,
            },
            sender,
        )
    }

    pub(crate) fn slot<T: 'static>(&self, h: Handle<T>) -> Rc<RefCell<T>> {
        self.slots[h.idx]
            .clone()
            .downcast::<RefCell<T>>()
            .expect("invariant: Handle<T> indexes a slot of type T")
    }

    fn new_slot<T: 'static>(&mut self, init: T) -> Rc<RefCell<T>> {
        let slot = Rc::new(RefCell::new(init));
        self.slots.push(slot.clone() as Rc<dyn Any>);
        slot
    }

    /// Register a node: its slot must already have been pushed (so slot and
    /// node indices stay aligned). `stop`/`teardown` default to no-ops; a node
    /// that needs them (e.g. `finally`) overwrites the field after pushing.
    fn push_node(
        &mut self,
        active_ups: Vec<usize>,
        activation: Activation,
        label: &'static str,
        cycle: CycleFn,
        start: LifecycleFn,
    ) {
        self.nodes.push(NodeRt {
            active_ups,
            activation,
            label,
            cycle,
            start,
            stop: Box::new(|_| Ok(())),
            teardown: Box::new(|_| Ok(())),
        });
        self.ticked.borrow_mut().push(false);
    }

    /// Shared cfg+state cell, used by both the cycle and start adapters.
    fn cell<C: 'static, S: 'static>(cfg: C, state: S) -> Rc<RefCell<(C, S)>> {
        Rc::new(RefCell::new((cfg, state)))
    }

    /// Register a **single-active-input** op — the shape shared by `map`,
    /// `fold`, `ewma`, and ~15 others: one upstream read by reference, one
    /// output slot, engine-owned `cfg`+`state`, no lifecycle hooks. This is the
    /// reusable core the `#[op]` attribute generates a thin wrapper around; the
    /// per-op `step` closure (which builds the concrete `(&a,)` input tuple and
    /// calls `Op::cycle`) is the only monomorphic piece, so this primitive
    /// stays free of the GAT-over-HRTB gymnastics a fully generic version would
    /// need. `label` is `type_name::<Op>()`; it is shortened for error context.
    pub(crate) fn register_op1<A, C, S, Out, Step>(
        &mut self,
        src: Handle<A>,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state: S,
        mut step: Step,
    ) -> Handle<Out>
    where
        A: 'static,
        C: 'static,
        S: 'static,
        Out: Default + 'static,
        Step: FnMut(&mut C, &mut S, &A, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(Out::default());
        let cs = Rc::new(RefCell::new((cfg, state)));
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
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub fn ticker(&mut self, period: Duration) -> Handle<()> {
        let idx = self.nodes.len();
        let out = self.new_slot(());
        let cs = Self::cell(NanoTime::from(period), None);
        let cs2 = cs.clone();
        self.push_node(
            Vec::new(),
            Ticker::ACTIVATION,
            "ticker",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                match Ticker::cycle(cfg, state, (), &mut ctx)? {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs2.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                Ticker::start(cfg, state, &mut ctx)
            }),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub fn constant<T: Clone + Default + 'static>(&mut self, value: T) -> Handle<T> {
        let idx = self.nodes.len();
        let out = self.new_slot(T::default());
        let cs = Self::cell(value, ());
        let cs2 = cs.clone();
        self.push_node(
            Vec::new(),
            Const::<T>::ACTIVATION,
            "constant",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                match Const::<T>::cycle(cfg, state, (), &mut ctx)? {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs2.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                Const::<T>::start(cfg, state, &mut ctx)
            }),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Pair each value with the current engine time: `(time, value)`. Kept
    /// hand-written (not `#[op]`): the output `(NanoTime, T)` is seeded from
    /// the input's current value, so it never requires `T: Default`.
    pub fn with_time<T: Clone + 'static>(&mut self, src: Handle<T>) -> Handle<(NanoTime, T)> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot((NanoTime::ZERO, src_slot.borrow().clone()));
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
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Rate-limit: emit at most once per `interval`.
    pub fn throttle<T: Clone + Default + 'static>(
        &mut self,
        src: Handle<T>,
        interval: Duration,
    ) -> Handle<T> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(T::default());
        let cs = Self::cell(NanoTime::from(interval), None::<NanoTime>);
        self.push_node(
            vec![src.idx],
            Throttle::<T>::ACTIVATION,
            "throttle",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match Throttle::<T>::cycle(cfg, state, (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Buffer values and flush them as a `Vec` on each `interval` boundary
    /// (and once more on the last cycle).
    pub fn window<T: Clone + Default + 'static>(
        &mut self,
        src: Handle<T>,
        interval: Duration,
    ) -> Handle<Vec<T>> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(Vec::<T>::new());
        let cs = Self::cell(NanoTime::from(interval), WindowState::<T>::default());
        let cs2 = cs.clone();
        self.push_node(
            vec![src.idx],
            Window::<T>::ACTIVATION,
            "window",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match Window::<T>::cycle(cfg, state, (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs2.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                Window::<T>::start(cfg, state, &mut ctx)
            }),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// The classic `trimap`: combine three streams, each independently active
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
        let mut active = Vec::with_capacity(3);
        if a_active {
            active.push(a.idx);
        }
        if b_active {
            active.push(b.idx);
        }
        if c_active {
            active.push(c.idx);
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
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub fn filter<T: Clone + Default + 'static>(
        &mut self,
        src: Handle<T>,
        condition: Handle<bool>,
    ) -> Handle<T> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let cond_slot = self.slot(condition);
        let out = self.new_slot(T::default());
        self.push_node(
            vec![src.idx, condition.idx],
            Filter::<T>::ACTIVATION,
            "filter",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                let v = src_slot.borrow();
                let c = cond_slot.borrow();
                match Filter::<T>::cycle(&mut (), &mut (), (&v, &c), &mut ctx)? {
                    Tick::Value(value) => {
                        drop(v);
                        *out.borrow_mut() = value;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub fn fold<A, B, F>(&mut self, src: Handle<A>, init: B, f: F) -> Handle<B>
    where
        A: 'static,
        B: Clone + 'static,
        F: Fn(&mut B, &A) + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(init.clone());
        let cs = Self::cell(f, init);
        self.push_node(
            vec![src.idx],
            Fold::<A, B, F>::ACTIVATION,
            "fold",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match Fold::<A, B, F>::cycle(cfg, state, (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Sample `src` (passively) whenever `trigger` ticks.
    pub fn sample<T: Clone + Default + 'static>(
        &mut self,
        src: Handle<T>,
        trigger: Handle<()>,
    ) -> Handle<T> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(T::default());
        self.push_node(
            vec![trigger.idx],
            Sample::<T>::ACTIVATION,
            "sample",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                let v = src_slot.borrow();
                match Sample::<T>::cycle(&mut (), &mut (), (&v,), &mut ctx)? {
                    Tick::Value(value) => {
                        drop(v);
                        *out.borrow_mut() = value;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Join two streams with a closure; ticks when either input ticks.
    pub fn join<A, B, C, F>(&mut self, a: Handle<A>, b: Handle<B>, f: F) -> Handle<C>
    where
        A: 'static,
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&A, &B) -> C + 'static,
    {
        self.bimap(a, true, b, true, f)
    }

    /// The classic `bimap`: combine two streams, each independently *active*
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
        let mut active = Vec::with_capacity(2);
        if a_active {
            active.push(a.idx);
        }
        if b_active {
            active.push(b.idx);
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
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Delay `src` by a fixed interval.
    pub fn delay<T: Clone + Default + PartialEq + 'static>(
        &mut self,
        src: Handle<T>,
        delay: Duration,
    ) -> Handle<T> {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(T::default());
        let ticked = self.ticked.clone();
        let is = src.idx;
        let cs = Self::cell(NanoTime::from(delay), DelayState::<T>::default());
        self.push_node(
            vec![src.idx],
            Delay::<T>::ACTIVATION,
            "delay",
            Box::new(move |k| {
                let src_ticked = ticked.borrow()[is];
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let v = src_slot.borrow();
                match Delay::<T>::cycle(cfg, state, (&v, src_ticked), &mut ctx)? {
                    Tick::Value(value) => {
                        drop(v);
                        *out.borrow_mut() = value;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Merge two streams; the earliest-supplied ticked input wins.
    pub fn merge2<T: Clone + Default + 'static>(
        &mut self,
        a: Handle<T>,
        b: Handle<T>,
    ) -> Handle<T> {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let out = self.new_slot(T::default());
        let ticked = self.ticked.clone();
        let (ia, ib) = (a.idx, b.idx);
        self.push_node(
            vec![a.idx, b.idx],
            Merge2::<T>::ACTIVATION,
            "merge",
            Box::new(move |k| {
                let (ta, tb) = {
                    let t = ticked.borrow();
                    (t[ia], t[ib])
                };
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                match Merge2::<T>::cycle(&mut (), &mut (), ((&va, ta), (&vb, tb)), &mut ctx)? {
                    Tick::Value(value) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = value;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
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
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Run `f` once at teardown — after the run ends, even if a cycle aborted
    /// it. Observes `src` (recording its last value) but emits nothing and
    /// never triggers downstream. Cleanup that must happen regardless of how
    /// the run terminated.
    pub fn finally<A, F>(&mut self, src: Handle<A>, f: F) -> Handle<()>
    where
        A: Clone + Default + 'static,
        F: Fn(&A) -> Result<()> + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(());
        let cs = Self::cell(f, A::default());
        let cs2 = cs.clone();
        self.push_node(
            vec![src.idx],
            Finally::<A, F>::ACTIVATION,
            "finally",
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match Finally::<A, F>::cycle(cfg, state, (&a,), &mut ctx)? {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(|_| Ok(())),
        );
        // Finally's whole purpose is its teardown hook.
        let node = self
            .nodes
            .last_mut()
            .expect("invariant: finally node just pushed");
        node.teardown = Box::new(move |k| {
            let (cfg, state) = &mut *cs2.borrow_mut();
            let mut ctx = Ctx::new(k, idx);
            Finally::<A, F>::teardown(cfg, state, &mut ctx)
        });
        Handle {
            idx,
            _t: PhantomData,
        }
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
        (
            Handle {
                idx,
                _t: PhantomData,
            },
            FeedbackSink { queue, source: idx },
        )
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
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    /// Mount a *composite* node: an entire compiled sub-graph behaving as a
    /// single node of this graph (the `graph!` macro's `nested` expansion).
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
        callback_activated: bool,
        node: F,
    ) -> Handle<T>
    where
        T: Clone + Default + 'static,
        F: FnMut(&mut Ctx, bool) -> Result<Tick<T>> + 'static,
    {
        let idx = self.nodes.len();
        let out = self.new_slot(T::default());
        let cell = Rc::new(RefCell::new(node));
        let cell2 = cell.clone();
        let caps = Activation {
            schedules: callback_activated,
            threaded: false,
            always: false,
        };
        self.push_node(
            active_ups,
            caps,
            "graph",
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                match (cell.borrow_mut())(&mut ctx, false)? {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        Ok(true)
                    }
                    Tick::Quiet => Ok(false),
                }
            }),
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                (cell2.borrow_mut())(&mut ctx, true)?;
                Ok(())
            }),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub(crate) fn ticked_rc(&self) -> Rc<RefCell<Vec<bool>>> {
        self.ticked.clone()
    }

    pub fn build(self) -> Runner {
        Runner {
            nodes: self.nodes,
            slots: self.slots,
            ticked: self.ticked,
            ready: self.ready,
            has_external: self.has_external,
            has_always: self.has_always,
            has_channel: self.has_channel,
        }
    }
}

/// Executes a wired graph. Dispatch walks nodes in wiring (topological)
/// order each cycle: a node runs when an active upstream ticked, or — only
/// for ops that declared [`Activation::schedules`](crate::op::Activation) — when the
/// kernel marked it dirty.
pub struct Runner {
    nodes: Vec<NodeRt>,
    slots: Vec<Rc<dyn Any>>,
    ticked: Rc<RefCell<Vec<bool>>>,
    ready: Option<ReadyReceiver>,
    has_external: bool,
    has_always: bool,
    has_channel: bool,
}

impl Runner {
    /// Run the graph to its bound. Returns the first error from any node's
    /// `start`/`cycle`/`stop`/`teardown` (with node context), or `Ok(())`.
    pub fn run(&mut self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        let realtime = matches!(run_mode, RunMode::RealTime);
        // `external`/`poll` are wall-clock (realtime-only); `channel` carries
        // timestamps and runs in both modes.
        assert!(
            realtime || !self.has_external,
            "graphs with external sources require RunMode::RealTime — untimestamped external \
             events have no place in a deterministic historical replay (use a channel with \
             timestamped sends for historical)"
        );
        assert!(
            realtime || !self.has_always,
            "graphs with poll sources require RunMode::RealTime — there is nothing to \
             busy-poll in a deterministic historical replay"
        );
        // The waker/ready channel is only used by realtime sources
        // (external, poll, realtime channel). A historical channel is
        // schedule-driven and needs no waker.
        let needs_waker = self.has_external || (self.has_channel && realtime);
        let mut kernel = if needs_waker {
            let ready = self
                .ready
                .take()
                .expect("a Runner with realtime sources supports a single run");
            Kernel::with_ready(run_mode, run_for, ready)
        } else {
            Kernel::new(run_mode, run_for)
        };
        if self.has_always {
            kernel.set_spin(true);
        }
        // First error (from start or a cycle) wins; `stop`/`teardown` still
        // run afterwards regardless, matching the classic engine.
        let mut first_err: Option<anyhow::Error> = None;

        for (i, node) in self.nodes.iter_mut().enumerate() {
            if let Err(e) = (node.start)(&mut kernel) {
                first_err = Some(e.context(format!("node {i} ({}) start", node.label)));
                break;
            }
        }

        if first_err.is_none() {
            let n = self.nodes.len();
            let mut dirty = vec![false; n];
            'run: while kernel.begin_cycle(&mut dirty) {
                for (i, node) in self.nodes.iter_mut().enumerate() {
                    let due = node.activation.always
                        || (node.activation.callback_activated() && dirty[i])
                        || {
                            let t = self.ticked.borrow();
                            node.active_ups.iter().any(|&u| t[u])
                        };
                    let did = if due {
                        match (node.cycle)(&mut kernel) {
                            Ok(did) => did,
                            Err(e) => {
                                first_err =
                                    Some(e.context(format!("node {i} ({}) cycle", node.label)));
                                break 'run;
                            }
                        }
                    } else {
                        false
                    };
                    self.ticked.borrow_mut()[i] = did;
                }
                for t in self.ticked.borrow_mut().iter_mut() {
                    *t = false;
                }
                kernel.end_cycle(&mut dirty);
            }
        }

        // Cleanup always runs; a stop/teardown error only surfaces if no
        // earlier error already won.
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if let Err(e) = (node.stop)(&mut kernel) {
                let e = e.context(format!("node {i} ({}) stop", node.label));
                first_err.get_or_insert(e);
            }
        }
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if let Err(e) = (node.teardown)(&mut kernel) {
                let e = e.context(format!("node {i} ({}) teardown", node.label));
                first_err.get_or_insert(e);
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Current value of a node's output slot.
    pub fn value<T: Clone + 'static>(&self, h: impl AsHandle<T>) -> T {
        let h = h.as_handle();
        self.slots[h.idx]
            .clone()
            .downcast::<RefCell<T>>()
            .expect("invariant: Handle<T> indexes a slot of type T")
            .borrow()
            .clone()
    }
}
