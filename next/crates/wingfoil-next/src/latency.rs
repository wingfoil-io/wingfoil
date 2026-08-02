//! Latency capture for wingfoil-next — the Phase 5 port of the classic
//! [`wingfoil::latency`] infrastructure.
//!
//! Stamp wall-clock timestamps onto messages as they hop through ops (and
//! across processes), then aggregate the per-stage deltas at the end of the
//! pipeline.
//!
//! # What is reused vs. new
//!
//! The **data layer is reused wholesale** from the classic crate — `Traced`
//! is just a `#[repr(C)]` payload, and the [`latency_stages!`] derive is
//! engine-agnostic, so they are re-exported unchanged (per the port-plan:
//! *"stamps ride values as today; latency_stages derive unchanged"*):
//!
//! - [`Traced<T, L>`] — payload `T` paired with a latency record `L`.
//! - [`Latency`] / [`Stage`] / [`HasLatency`] — the record traits.
//! - [`StageStats`] / [`LatencyStats`] — the non-allocating aggregators.
//! - [`latency_stages!`] — declares a record + per-stage marker types.
//!
//! Only the **node layer** is re-implemented as [`Op`]s on the next engine:
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
//! # Deviation from classic
//!
//! Exposed via the **fluent (interpreted)** path only, matching classic
//! (which offers latency solely through `LatencyStreamOps`). A stamp's stage
//! is a compile-time *type* parameter, which does not map onto the
//! `nitro!`/compiled value-dispatch table; compiled/nested support is out of
//! scope for this op family.
//!
//! # Example
//!
//! ```ignore
//! use wingfoil_next::prelude::*;
//! use wingfoil_next::latency::*;
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

use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};

// The pure data layer is engine-agnostic and reused verbatim from the classic
// crate (its serde/iceoryx2 impls are already compiled there).
pub use wingfoil::{HasLatency, Latency, LatencyStats, Stage, StageStats, Traced, latency_stages};

// ---------------------------------------------------------------------------
// Stamp / StampPrecise — pass-through ops that stamp one stage
// ---------------------------------------------------------------------------

/// Op: forward the payload unchanged while stamping
/// [`Ctx::wall_time`](crate::op::Ctx::wall_time) (cycle-start snap) into a
/// single named stage `S` of the embedded [`Latency`] record. One `u64` store
/// per tick, no allocation. The next twin of classic `StampStream`.
pub struct Stamp<P, S>(PhantomData<fn() -> (P, S)>);

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
/// distinct timestamps. The next twin of classic `StampPreciseStream`.
pub struct StampPrecise<P, S>(PhantomData<fn() -> (P, S)>);

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
// LatencyReport — sink op aggregating per-stage delta statistics
// ---------------------------------------------------------------------------

/// Construction config for a [`LatencyReport`] sink: the shared stats
/// accumulator plus whether to print a summary at [`stop`](Op::stop).
pub struct LatencyReportCfg<L: Latency> {
    pub stats: Rc<RefCell<LatencyStats<L>>>,
    pub print_on_teardown: bool,
}

/// Sink op consuming a stream of `P: HasLatency`, accumulating per-stage delta
/// statistics into a shared [`LatencyStats`]. At [`stop`](Op::stop) it prints
/// the summary when configured. The next twin of classic `LatencyReport`.
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
