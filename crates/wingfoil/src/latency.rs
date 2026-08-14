//! Latency capture for wingfoil — the Phase 5 port of the legacy
//! [`wingfoil::latency`] infrastructure.
//!
//! Stamp wall-clock timestamps onto messages as they hop through ops (and
//! across processes), then aggregate the per-stage deltas at the end of the
//! pipeline.
//!
//! # What is reused vs. new
//!
//! The **data layer is reused wholesale** from the legacy crate — `Traced`
//! is just a `#[repr(C)]` payload, and the [`latency_stages!`] derive is
//! engine-agnostic, so they are re-exported unchanged (per the port-plan:
//! *"stamps ride values as today; latency_stages derive unchanged"*):
//!
//! - [`Traced<T, L>`] — payload `T` paired with a latency record `L`.
//! - [`Latency`] / [`Stage`] / [`HasLatency`] — the record traits.
//! - [`StageStats`] / [`LatencyStats`] — the non-allocating aggregators, plus
//!   [`record_stage_deltas`] / [`format_latency_report`], the same aggregation
//!   and report format over a *runtime* stage list (what the Python bindings
//!   aggregate on, since Python cannot name a compile-time [`Stage`]).
//! - [`latency_stages!`] — declares a record + per-stage marker types.
//!
//! Only the **node layer** is re-implemented as [`Op`]s on the wingfoil engine:
//! [`LatencyStreamOps::stamp`] / [`stamp_precise`](LatencyStreamOps::stamp_precise)
//! and [`LatencyReportOps::latency_report`].
//!
//! # Time source
//!
//! Stamps always read the wall clock (never [`Ctx::time`](crate::op::Ctx::time),
//! which is source-driven in historical mode and useless for latency):
//!
//! - [`stamp`](LatencyStreamOps::stamp) reads
//!   [`Ctx::wall_time`](crate::op::Ctx::wall_time) — a cycle-start snap, one
//!   `u64` load. Stages sharing an engine cycle get the same stamp.
//! - [`stamp_precise`](LatencyStreamOps::stamp_precise) reads
//!   [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise) — a fresh
//!   TSC read, giving distinct stamps to stages in the same cycle.
//!
//! Both behave identically in realtime and historical mode.
//!
//! # Toggling
//!
//! Each method has an `_if` variant taking a bool and returning the upstream
//! unchanged when disabled — no node inserted, zero runtime cost.
//!
//! # Tiers
//!
//! [`stamp`](LatencyStreamOps::stamp) and
//! [`stamp_precise`](LatencyStreamOps::stamp_precise) work in **all three**
//! `nitro!` expansions — `interpreted()`, `compiled()` and `nested()`.
//!
//! Getting there needed a macro feature. A stamp's stage is a compile-time
//! *type* (`stamp::<trade_latency::ingest>()`), and `nitro!`'s dispatch
//! forwards *values* — there is no value to forward. It also cannot be a
//! turbofish on the generated forwarder: Rust wants all of a function's type
//! arguments or none, and the macro never learns the forwarder's arity, since
//! never naming the op type is the whole point of the naming-convention
//! design. So the stage crosses as a **value whose type carries it**:
//! `#[op(explicit = S)]` gives each forwarder a leading `PhantomData<S>`, and
//! the emission passes `PhantomData::<the_stage>` so inference resolves the
//! parameter from an argument like any other. Same deferral trick as passing a
//! literal closure by value for `cycle_owned_cfg`.
//!
//! [`latency_report`](LatencyReportOps::latency_report) stays
//! **interpreted-only**, and that one is structural rather than a macro gap:
//! the sink's whole value is the `Rc<RefCell<LatencyStats>>` handle it hands
//! back, and `compiled()` is outputs-only by design — a closed box that
//! returns its declared output values and nothing else. There is no way for
//! the handle to escape it, so a compiled `latency_report` could only ever
//! print at teardown, never be read. Deviation register **C7**.
//!
//! # Burst-shaped forms
//!
//! Adapters emit `Stream<Burst<T>>`, and [`collapse`](crate::ops::Collapse)
//! — the one-step bridge to the scalar combinators — keeps only the burst's
//! **last** value. On an ingest path carrying events rather than a
//! latest-wins signal that is silent data loss, and it only appears once a
//! producer outruns the graph cycle. So every op here has a burst-shaped form:
//!
//! - [`LatencyBurstStreamOps::stamp_each`] /
//!   [`stamp_precise_each`](LatencyBurstStreamOps::stamp_precise_each) — the
//!   clock is read **once per burst**, since a burst is one instant and a
//!   per-value read would invent differences that do not exist.
//! - [`LatencyReportOps`] has a `Stream<Burst<P>>` impl under the *same* method
//!   name, observing every value in the burst.
//!
//! The asymmetry in naming is forced, not stylistic: `latency_report` can share
//! its name because the trait is generic over `P` (so the two impls never
//! overlap) and it has no `nitro!` forwarder to collide; the stamps cannot,
//! because `nitro!` dispatches forwarders off the method-name token alone and
//! both stamp shapes are dual-mode. Prefer the shared-name shape when adding
//! burst support elsewhere — a suffix is a cost every caller pays. When a
//! constraint does force one, follow the suffix convention: `_each` means
//! *per value in the burst* (these stamps, the web adapter's `web_pub_each`),
//! `_bursts` means *the whole group as one atomic unit* (`web_pub_bursts`).
//!
//! # Deviation from legacy
//!
//! None for the tier surface: legacy offers latency solely through
//! `LatencyStreamOps`, so wingfoil is a superset here — and the burst-shaped
//! forms above have no legacy equivalent at all.
//!
//! # Example
//!
//! ```ignore
//! use wingfoil::prelude::*;
//! use wingfoil::latency::*;
//!
//! latency_stages! {
//!     pub TradeLatency { ingest, decode, strategy, publish }
//! }
//!
//! fn build(stream: Stream<Traced<u64, TradeLatency>>) -> Stream<Traced<u64, TradeLatency>> {
//!     stream
//!         .stamp::<trade_latency::ingest>()
//!         .stamp::<trade_latency::strategy>()
//! }
//! ```

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use anyhow::Result;

use crate::Burst;
use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};
use wingfoil_derive::op;

// The pure data layer is engine-agnostic and lives in `runtime::latency`,
// shared with the legacy crate (which re-exports it from here).
pub use crate::runtime::latency::{
    HasLatency, Latency, LatencyStats, Stage, StageStats, Traced, format_latency_report,
    latency_stages, record_stage_deltas,
};

// ---------------------------------------------------------------------------
// Stamp / StampPrecise — pass-through ops that stamp one stage
// ---------------------------------------------------------------------------

/// Op: forward the payload unchanged while stamping
/// [`Ctx::wall_time`](crate::op::Ctx::wall_time) (cycle-start snap) into a
/// single named stage `S` of the embedded [`Latency`] record. One `u64` store
/// per tick, no allocation. The wingfoil twin of legacy `StampStream`.
pub struct Stamp<P, S>(PhantomData<fn() -> (P, S)>);

// `no_builder`: the fluent surface is the hand-written `LatencyStreamOps`
// trait below, which already carries the `::<S>` turbofish and the
// `HasLatency` bound. What `#[op]` is here for is the *forwarders* — the
// `__wf_op_stamp_*` family the `nitro!` compiled and nested emissions dispatch
// through, which is the whole of gap C7.
//
// `explicit = S`: the stage is a compile-time type and appears in no argument,
// so inference cannot reach it; hoisting it to the front of the forwarder
// signatures lets the emission prefix the call-site turbofish. This is the
// mechanism the deviation register said `nitro!` lacked.
#[op(build = stamp, no_builder, explicit = S)]
impl<P, S> Op for Stamp<P, S>
where
    P: Clone + Default + HasLatency + 'static,
    S: Stage<P::L> + 'static,
{
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a P,);
    type Out = P;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&P,), ctx: &mut Ctx<'_>) -> Result<Tick<P>> {
        let mut value = input.0.clone();
        S::stamp(value.latency_mut(), u64::from(ctx.wall_time()));
        Ok(Tick::Value(value))
    }
}

/// Like [`Stamp`] but reads
/// [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise) — a fresh TSC
/// snap on every tick — so stages running in the same engine cycle get
/// distinct timestamps. The wingfoil twin of legacy `StampPreciseStream`.
pub struct StampPrecise<P, S>(PhantomData<fn() -> (P, S)>);

#[op(build = stamp_precise, no_builder, explicit = S)]
impl<P, S> Op for StampPrecise<P, S>
where
    P: Clone + Default + HasLatency + 'static,
    S: Stage<P::L> + 'static,
{
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a P,);
    type Out = P;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&P,), ctx: &mut Ctx<'_>) -> Result<Tick<P>> {
        let mut value = input.0.clone();
        S::stamp(value.latency_mut(), u64::from(ctx.wall_time_precise()));
        Ok(Tick::Value(value))
    }
}

/// Extension trait adding `.stamp::<Stage>()` and friends to streams whose
/// values carry a [`Latency`] record.
pub trait LatencyStreamOps<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    /// Wrap in a [`Stamp`] op for stage `S`: each tick writes
    /// [`Ctx::wall_time`](crate::op::Ctx::wall_time) into the stage's slot
    /// before forwarding.
    #[must_use]
    fn stamp<S: Stage<P::L> + 'static>(&self) -> Stream<P>;

    /// Conditional [`stamp`](Self::stamp): when `enabled` is false returns
    /// `self` unchanged — no node inserted, zero runtime cost.
    #[must_use]
    fn stamp_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<P>;

    /// Like [`stamp`](Self::stamp) but uses
    /// [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise) for
    /// intra-cycle resolution.
    #[must_use]
    fn stamp_precise<S: Stage<P::L> + 'static>(&self) -> Stream<P>;

    /// Conditional [`stamp_precise`](Self::stamp_precise).
    #[must_use]
    fn stamp_precise_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<P>;
}

impl<P> LatencyStreamOps<P> for Stream<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    fn stamp<S: Stage<P::L> + 'static>(&self) -> Stream<P> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "stamp",
                Activation::NONE,
                (),
                || (),
                move |cfg: &mut (), state: &mut (), value: &P, ctx: &mut Ctx<'_>| {
                    Stamp::<P, S>::cycle(cfg, state, (value,), ctx)
                },
            )
        })
    }

    fn stamp_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<P> {
        if enabled {
            self.stamp::<S>()
        } else {
            self.clone()
        }
    }

    fn stamp_precise<S: Stage<P::L> + 'static>(&self) -> Stream<P> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "stamp_precise",
                Activation::NONE,
                (),
                || (),
                move |cfg: &mut (), state: &mut (), value: &P, ctx: &mut Ctx<'_>| {
                    StampPrecise::<P, S>::cycle(cfg, state, (value,), ctx)
                },
            )
        })
    }

    fn stamp_precise_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<P> {
        if enabled {
            self.stamp_precise::<S>()
        } else {
            self.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Burst stamps — stamp every value in a same-instant group
// ---------------------------------------------------------------------------
//
// The scalar stamps above take one value per cycle, which forces any pipeline
// that wants to stamp adapter output to `collapse()` its `Burst` first — and
// `collapse` keeps only the burst's **last** item. On an ingest path carrying
// orders, fills or control messages that is silent data loss, and it strikes
// exactly under load, when the producer outruns the graph cycle and bursts
// stop being single-item. These ops are how a stamped pipeline stays
// burst-shaped end to end instead.
//
// The clock is read **once per burst**, not once per value: a burst is by
// definition one instant's worth of values, so a per-value read would invent
// differences that do not exist. `stamp_precise_each` still gives distinct
// timestamps to distinct *stages* in the same cycle, which is what precise
// stamping is for.

/// Op: forward a [`Burst`] unchanged while stamping
/// [`Ctx::wall_time`](crate::op::Ctx::wall_time) into stage `S` of **every**
/// value it carries. The burst-shaped twin of [`Stamp`].
pub struct StampEach<P, S>(PhantomData<fn() -> (P, S)>);

#[op(build = stamp_each, no_builder, explicit = S)]
impl<P, S> Op for StampEach<P, S>
where
    P: Clone + Default + HasLatency + 'static,
    S: Stage<P::L> + 'static,
{
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a Burst<P>,);
    type Out = Burst<P>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&Burst<P>,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<P>>> {
        let now = u64::from(ctx.wall_time());
        let mut out = input.0.clone();
        for value in out.iter_mut() {
            S::stamp(value.latency_mut(), now);
        }
        Ok(Tick::Value(out))
    }
}

/// Op: [`StampEach`] reading
/// [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise) — one fresh
/// TSC snap per burst, so stages sharing an engine cycle get distinct
/// timestamps. The burst-shaped twin of [`StampPrecise`].
pub struct StampPreciseEach<P, S>(PhantomData<fn() -> (P, S)>);

#[op(build = stamp_precise_each, no_builder, explicit = S)]
impl<P, S> Op for StampPreciseEach<P, S>
where
    P: Clone + Default + HasLatency + 'static,
    S: Stage<P::L> + 'static,
{
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a Burst<P>,);
    type Out = Burst<P>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&Burst<P>,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<P>>> {
        let now = u64::from(ctx.wall_time_precise());
        let mut out = input.0.clone();
        for value in out.iter_mut() {
            S::stamp(value.latency_mut(), now);
        }
        Ok(Tick::Value(out))
    }
}

/// Extension trait adding the burst-shaped stamps to a `Stream<Burst<P>>`.
///
/// The method names carry `_each` rather than overloading `stamp` on a second
/// receiver type. Two ops cannot share one name: `nitro!` dispatches its
/// forwarders off the method-name token alone (`__wf_op_stamp_cycle`), so a
/// single name for both shapes would be unresolvable in a compiled or nested
/// expansion. Paying one word at the call site keeps the fluent and `nitro!`
/// spellings identical, which is the rule `logged` is the standing exception
/// to. `_each` — stamp **each** value — follows the suffix convention set by
/// the web adapter: `_each` is per value, `_bursts` is one atomic group.
pub trait LatencyBurstStreamOps<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    /// Stamp stage `S` on every value in each burst, from the cycle-start
    /// wall-clock snap.
    #[must_use]
    fn stamp_each<S: Stage<P::L> + 'static>(&self) -> Stream<Burst<P>>;

    /// Conditional [`stamp_each`](Self::stamp_each): when `enabled` is false
    /// returns `self` unchanged — no node inserted, zero runtime cost.
    #[must_use]
    fn stamp_each_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<Burst<P>>;

    /// Stamp stage `S` on every value in each burst, from a fresh TSC read.
    #[must_use]
    fn stamp_precise_each<S: Stage<P::L> + 'static>(&self) -> Stream<Burst<P>>;

    /// Conditional [`stamp_precise_each`](Self::stamp_precise_each).
    #[must_use]
    fn stamp_precise_each_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<Burst<P>>;
}

impl<P> LatencyBurstStreamOps<P> for Stream<Burst<P>>
where
    P: Clone + Default + HasLatency + 'static,
{
    fn stamp_each<S: Stage<P::L> + 'static>(&self) -> Stream<Burst<P>> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "stamp_each",
                Activation::NONE,
                (),
                || (),
                move |cfg: &mut (), state: &mut (), value: &Burst<P>, ctx: &mut Ctx<'_>| {
                    StampEach::<P, S>::cycle(cfg, state, (value,), ctx)
                },
            )
        })
    }

    fn stamp_each_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<Burst<P>> {
        if enabled {
            self.stamp_each::<S>()
        } else {
            self.clone()
        }
    }

    fn stamp_precise_each<S: Stage<P::L> + 'static>(&self) -> Stream<Burst<P>> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "stamp_precise_each",
                Activation::NONE,
                (),
                || (),
                move |cfg: &mut (), state: &mut (), value: &Burst<P>, ctx: &mut Ctx<'_>| {
                    StampPreciseEach::<P, S>::cycle(cfg, state, (value,), ctx)
                },
            )
        })
    }

    fn stamp_precise_each_if<S: Stage<P::L> + 'static>(&self, enabled: bool) -> Stream<Burst<P>> {
        if enabled {
            self.stamp_precise_each::<S>()
        } else {
            self.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// LatencyReport — sink op aggregating per-stage delta statistics
// ---------------------------------------------------------------------------

/// Construction config for a [`LatencyReport`] sink: the shared stats
/// accumulator plus whether to print a summary at [`stop`](Op::stop).
pub struct LatencyReportCfg<L: Latency> {
    /// The accumulator the sink folds each observation into. Shared, so the
    /// caller can read the numbers out after the run.
    pub stats: Rc<RefCell<LatencyStats<L>>>,
    /// Print the per-stage summary to stdout when the run stops.
    pub print_on_teardown: bool,
}

/// Sink op consuming a stream of `P: HasLatency`, accumulating per-stage delta
/// statistics into a shared [`LatencyStats`]. At [`stop`](Op::stop) it prints
/// the summary when configured. The wingfoil twin of legacy `LatencyReport`.
pub struct LatencyReport<P>(PhantomData<fn() -> P>);

impl<P> Op for LatencyReport<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    type Cfg = LatencyReportCfg<P::L>;
    type State = ();
    type In<'a> = (&'a P,);
    type Out = ();
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut Self::Cfg,
        _state: &mut (),
        input: (&P,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<()>> {
        cfg.stats.borrow_mut().observe(input.0.latency());
        Ok(Tick::Value(()))
    }

    fn stop(cfg: &mut Self::Cfg, _state: &mut (), _ctx: &mut Ctx<'_>) -> Result<()> {
        if cfg.print_on_teardown {
            print!("{}", cfg.stats.borrow().format_report());
        }
        Ok(())
    }
}

/// Extension methods to install a [`LatencyReport`] sink. Returns the sink
/// stream plus the shared stats handle, so the caller can inspect the numbers
/// after the run.
pub trait LatencyReportOps<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    /// Install a [`LatencyReport`] sink. `print_on_teardown` controls whether
    /// a summary is printed at shutdown. Returns `(sink_stream, stats_handle)`.
    fn latency_report(
        &self,
        print_on_teardown: bool,
    ) -> (Stream<()>, Rc<RefCell<LatencyStats<P::L>>>);

    /// Conditional variant. When `enabled` is false, installs a sink that
    /// never ticks and returns an empty stats handle (counts stay at zero) —
    /// letting a single config flag toggle aggregation without wiring edits.
    fn latency_report_if(
        &self,
        enabled: bool,
        print_on_teardown: bool,
    ) -> (Stream<()>, Rc<RefCell<LatencyStats<P::L>>>);
}

impl<P> LatencyReportOps<P> for Stream<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    fn latency_report(
        &self,
        print_on_teardown: bool,
    ) -> (Stream<()>, Rc<RefCell<LatencyStats<P::L>>>) {
        let stats = Rc::new(RefCell::new(LatencyStats::new()));
        let stats_for_wire = stats.clone();
        let stream = self.wire(move |b, h| b.latency_report(h, print_on_teardown, stats_for_wire));
        (stream, stats)
    }

    fn latency_report_if(
        &self,
        enabled: bool,
        print_on_teardown: bool,
    ) -> (Stream<()>, Rc<RefCell<LatencyStats<P::L>>>) {
        if enabled {
            self.latency_report(print_on_teardown)
        } else {
            // A source that never ticks: nothing is observed, stats stay empty.
            let stats = Rc::new(RefCell::new(LatencyStats::new()));
            let stream = self.wire(|b, _h| b.never());
            (stream, stats)
        }
    }
}

// ---------------------------------------------------------------------------
// LatencyReportEach — the same sink over a burst-shaped stream
// ---------------------------------------------------------------------------

/// Sink op consuming a stream of [`Burst`]s, observing **every** value in each
/// burst into the shared [`LatencyStats`]. The burst-shaped twin of
/// [`LatencyReport`], and the reason a stamped pipeline no longer has to
/// `collapse()` — which would drop all but the last value of each burst, and
/// with it every latency sample those values carried.
pub struct LatencyReportEach<P>(PhantomData<fn() -> P>);

impl<P> Op for LatencyReportEach<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    type Cfg = LatencyReportCfg<P::L>;
    type State = ();
    type In<'a> = (&'a Burst<P>,);
    type Out = ();
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut Self::Cfg,
        _state: &mut (),
        input: (&Burst<P>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<()>> {
        let mut stats = cfg.stats.borrow_mut();
        for value in input.0.iter() {
            stats.observe(value.latency());
        }
        Ok(Tick::Value(()))
    }

    fn stop(cfg: &mut Self::Cfg, _state: &mut (), _ctx: &mut Ctx<'_>) -> Result<()> {
        if cfg.print_on_teardown {
            print!("{}", cfg.stats.borrow().format_report());
        }
        Ok(())
    }
}

/// The burst-shaped report: **the same trait and the same method names**,
/// selected by the receiver's shape. `stream.latency_report(true)` means
/// "observe the value" on a `Stream<P>` and "observe every value in the burst"
/// on a `Stream<Burst<P>>`.
///
/// Worth contrasting with [`LatencyBurstStreamOps`], which had to invent
/// `_each` names. Two things make the shared name possible here and not
/// there:
///
/// * [`LatencyReportOps<P>`] is generic over `P`, so `Stream<P>` and
///   `Stream<Burst<P>>` instantiate the trait at *different* `P` and can never
///   overlap. (A non-generic trait cannot do this — see
///   [`WebBurstSinkOps`](crate::adapters::web::WebBurstSinkOps), which is a
///   separate trait precisely because `WebSinkOps` is not generic.)
/// * `latency_report` is interpreted-only by structure, so there is no
///   `nitro!` forwarder to collide. The stamps *are* dual-mode, and `nitro!`
///   dispatches forwarders off the method-name token alone, so one name there
///   could not resolve to two ops.
///
/// Prefer this shape when adding burst support to an op: a suffix is a cost
/// paid by every caller, and it is only worth paying when one of the two
/// constraints above forces it.
impl<P> LatencyReportOps<P> for Stream<Burst<P>>
where
    P: Clone + Default + HasLatency + 'static,
{
    fn latency_report(
        &self,
        print_on_teardown: bool,
    ) -> (Stream<()>, Rc<RefCell<LatencyStats<P::L>>>) {
        let stats = Rc::new(RefCell::new(LatencyStats::new()));
        let stats_for_wire = stats.clone();
        let stream =
            self.wire(move |b, h| b.latency_report_each(h, print_on_teardown, stats_for_wire));
        (stream, stats)
    }

    fn latency_report_if(
        &self,
        enabled: bool,
        print_on_teardown: bool,
    ) -> (Stream<()>, Rc<RefCell<LatencyStats<P::L>>>) {
        if enabled {
            self.latency_report(print_on_teardown)
        } else {
            // A source that never ticks: nothing is observed, stats stay empty.
            let stats = Rc::new(RefCell::new(LatencyStats::new()));
            let stream = self.wire(|b, _h| b.never());
            (stream, stats)
        }
    }
}
