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
//! Prototype simplifications (a production version would differ): value
//! slots are individual `Rc<RefCell<T>>`s rather than a contiguous arena,
//! dispatch walks nodes in wiring (topological) order rather than using
//! dirty-lists, and `run` is infallible because the prototype ops are.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use crate::op::{Ctx, Op, Tick};
use crate::ops::{Const, Delay, DelayState, Filter, Fold, Join, Map, Merge2, Sample, Ticker};
use wingfoil::codegen::Kernel;
use wingfoil::{NanoTime, RunFor, RunMode};

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

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Handle<T> {}

type CycleFn = Box<dyn FnMut(&mut Kernel) -> bool>;
type StartFn = Box<dyn FnMut(&mut Kernel)>;

struct NodeRt {
    active_ups: Vec<usize>,
    /// From `Op::CAPS`: whether this node can be activated by a scheduled
    /// callback. Nodes without it skip the dirty check entirely — the
    /// capability contract doing the job of the retrofit's name allowlist.
    schedules: bool,
    cycle: CycleFn,
    start: StartFn,
}

/// Wires a graph of [`Op`]s. Combinators mirror the classic fluent API but
/// the engine — not the node — owns state, config and values.
#[derive(Default)]
pub struct Builder {
    nodes: Vec<NodeRt>,
    slots: Vec<Rc<dyn Any>>,
    ticked: Rc<RefCell<Vec<bool>>>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot<T: 'static>(&self, h: Handle<T>) -> Rc<RefCell<T>> {
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
    /// node indices stay aligned).
    fn push_node(
        &mut self,
        active_ups: Vec<usize>,
        schedules: bool,
        cycle: CycleFn,
        start: StartFn,
    ) {
        self.nodes.push(NodeRt {
            active_ups,
            schedules,
            cycle,
            start,
        });
        self.ticked.borrow_mut().push(false);
    }

    /// Shared cfg+state cell, used by both the cycle and start adapters.
    fn cell<C: 'static, S: 'static>(cfg: C, state: S) -> Rc<RefCell<(C, S)>> {
        Rc::new(RefCell::new((cfg, state)))
    }

    pub fn ticker(&mut self, period: Duration) -> Handle<()> {
        let idx = self.nodes.len();
        let out = self.new_slot(());
        let cs = Self::cell(NanoTime::from(period), None);
        let cs2 = cs.clone();
        self.push_node(
            Vec::new(),
            Ticker::CAPS.schedules,
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                match Ticker::cycle(cfg, state, (), &mut ctx) {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs2.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                Ticker::start(cfg, state, &mut ctx);
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
            Const::<T>::CAPS.schedules,
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                match Const::<T>::cycle(cfg, state, (), &mut ctx) {
                    Tick::Value(v) => {
                        *out.borrow_mut() = v;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(move |k| {
                let (cfg, state) = &mut *cs2.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                Const::<T>::start(cfg, state, &mut ctx);
            }),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub fn map<A, B, F>(&mut self, src: Handle<A>, f: F) -> Handle<B>
    where
        A: 'static,
        B: Clone + Default + 'static,
        F: FnMut(&A) -> B + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(B::default());
        let cs = Self::cell(f, ());
        self.push_node(
            vec![src.idx],
            Map::<A, B, F>::CAPS.schedules,
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match Map::<A, B, F>::cycle(cfg, state, (&a,), &mut ctx) {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
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
            Filter::<T>::CAPS.schedules,
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                let v = src_slot.borrow();
                let c = cond_slot.borrow();
                match Filter::<T>::cycle(&mut (), &mut (), (&v, &c), &mut ctx) {
                    Tick::Value(value) => {
                        drop(v);
                        *out.borrow_mut() = value;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
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
        F: FnMut(&mut B, &A) + 'static,
    {
        let idx = self.nodes.len();
        let src_slot = self.slot(src);
        let out = self.new_slot(init.clone());
        let cs = Self::cell(f, init);
        self.push_node(
            vec![src.idx],
            Fold::<A, B, F>::CAPS.schedules,
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let a = src_slot.borrow();
                match Fold::<A, B, F>::cycle(cfg, state, (&a,), &mut ctx) {
                    Tick::Value(v) => {
                        drop(a);
                        *out.borrow_mut() = v;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
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
            Sample::<T>::CAPS.schedules,
            Box::new(move |k| {
                let mut ctx = Ctx::new(k, idx);
                let v = src_slot.borrow();
                match Sample::<T>::cycle(&mut (), &mut (), (&v,), &mut ctx) {
                    Tick::Value(value) => {
                        drop(v);
                        *out.borrow_mut() = value;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
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
        F: FnMut(&A, &B) -> C + 'static,
    {
        let idx = self.nodes.len();
        let a_slot = self.slot(a);
        let b_slot = self.slot(b);
        let out = self.new_slot(C::default());
        let cs = Self::cell(f, ());
        self.push_node(
            vec![a.idx, b.idx],
            Join::<A, B, C, F>::CAPS.schedules,
            Box::new(move |k| {
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                match Join::<A, B, C, F>::cycle(cfg, state, (&va, &vb), &mut ctx) {
                    Tick::Value(v) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = v;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
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
            Delay::<T>::CAPS.schedules,
            Box::new(move |k| {
                let src_ticked = ticked.borrow()[is];
                let (cfg, state) = &mut *cs.borrow_mut();
                let mut ctx = Ctx::new(k, idx);
                let v = src_slot.borrow();
                match Delay::<T>::cycle(cfg, state, (&v, src_ticked), &mut ctx) {
                    Tick::Value(value) => {
                        drop(v);
                        *out.borrow_mut() = value;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
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
            Merge2::<T>::CAPS.schedules,
            Box::new(move |k| {
                let (ta, tb) = {
                    let t = ticked.borrow();
                    (t[ia], t[ib])
                };
                let mut ctx = Ctx::new(k, idx);
                let va = a_slot.borrow();
                let vb = b_slot.borrow();
                match Merge2::<T>::cycle(&mut (), &mut (), ((&va, ta), (&vb, tb)), &mut ctx) {
                    Tick::Value(value) => {
                        drop(va);
                        drop(vb);
                        *out.borrow_mut() = value;
                        true
                    }
                    Tick::Quiet => false,
                }
            }),
            Box::new(|_| {}),
        );
        Handle {
            idx,
            _t: PhantomData,
        }
    }

    pub fn build(self) -> Runner {
        Runner {
            nodes: self.nodes,
            slots: self.slots,
            ticked: self.ticked,
        }
    }
}

/// Executes a wired graph. Dispatch walks nodes in wiring (topological)
/// order each cycle: a node runs when an active upstream ticked, or — only
/// for ops that declared [`Caps::schedules`](crate::op::Caps) — when the
/// kernel marked it dirty.
pub struct Runner {
    nodes: Vec<NodeRt>,
    slots: Vec<Rc<dyn Any>>,
    ticked: Rc<RefCell<Vec<bool>>>,
}

impl Runner {
    pub fn run(&mut self, run_mode: RunMode, run_for: RunFor) {
        let mut kernel = Kernel::new(run_mode, run_for);
        for node in self.nodes.iter_mut() {
            (node.start)(&mut kernel);
        }
        let n = self.nodes.len();
        let mut dirty = vec![false; n];
        while kernel.begin_cycle(&mut dirty) {
            for (i, node) in self.nodes.iter_mut().enumerate() {
                let due = (node.schedules && dirty[i]) || {
                    let t = self.ticked.borrow();
                    node.active_ups.iter().any(|&u| t[u])
                };
                let did = due && (node.cycle)(&mut kernel);
                self.ticked.borrow_mut()[i] = did;
            }
            for t in self.ticked.borrow_mut().iter_mut() {
                *t = false;
            }
            kernel.end_cycle(&mut dirty);
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
