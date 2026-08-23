//! Latency capture for wingfoil — stamp wall-clock timestamps onto messages as
//! they hop through ops (and across processes), then aggregate the per-stage
//! deltas at the end of the pipeline.
//!
//! # The surface in one screen
//!
//! ```ignore
//! use wingfoil::prelude::*;
//! use wingfoil::latency::*;
//!
//! latency_stages! {
//!     pub TradeLatency { ingest, decode, strategy, publish }
//! }
//!
//! // One stage, cycle-start clock:
//! let s = stream.stamp::<trade_latency::ingest>();
//! // One stage, mode chosen at runtime — Off / Cycle / Precise:
//! let s = s.stamp_as::<trade_latency::decode>(mode);
//! // Several stages in one node — one clone instead of N:
//! let s = s.stamp_all::<(trade_latency::strategy, trade_latency::publish)>(mode);
//! // Aggregate, and read the numbers however you like:
//! let (_sink, latency) = s.latency_report(ReportOutput::Stdout);
//! let per_second = latency.windows(&g, Duration::from_secs(1));
//! ```
//!
//! # What is reused vs. new
//!
//! The **data layer is engine-agnostic** and lives in
//! [`runtime::latency`](crate::runtime::latency), re-exported here:
//!
//! - [`Traced<T, L>`] — payload `T` paired with a latency record `L`.
//! - [`Latency`] / [`Stage`] / [`HasLatency`] — the record traits.
//! - [`StageStats`] / [`LatencyStats`] — the non-allocating aggregators, plus
//!   [`HopStats`] / [`LatencySnapshot`], their small `Copy` read-outs, and
//!   [`record_stage_deltas`] / [`format_latency_report`], the same aggregation
//!   and report format over a *runtime* stage list (what the Python bindings
//!   aggregate on, since Python cannot name a compile-time [`Stage`]).
//! - [`latency_stages!`] — declares a record + per-stage marker types.
//!
//! Only the **node layer** is implemented as [`Op`]s on the wingfoil engine:
//! the stamp family and [`LatencyReportOps::latency_report`].
//!
//! # Time source
//!
//! Stamps always read the wall clock (never [`Ctx::time`](crate::op::Ctx::time),
//! which is source-driven in historical mode and useless for latency). Which
//! clock is [`Stamping`]:
//!
//! - [`Stamping::Cycle`] reads [`Ctx::wall_time`](crate::op::Ctx::wall_time) —
//!   a cycle-start snap, one `u64` load. Free, but **stages sharing an engine
//!   cycle get the same stamp**, so the hop between them is not measured at
//!   all. The report says so rather than reporting a zero
//!   ([`StageStats::same_instant`]).
//! - [`Stamping::Precise`] reads
//!   [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise) — a fresh
//!   TSC read (~5–10 ns), giving distinct stamps to stages in the same cycle.
//! - [`Stamping::Off`] inserts no node at all.
//!
//! Both clocks behave identically in realtime and historical mode.
//!
//! # One mode argument, not a method per combination
//!
//! Each stamp used to carry a plain form (`stamp`), a precise form
//! (`stamp_precise`) and an `_if` form of each — four methods per stream shape,
//! which still could not express *"precise or not, decided at runtime"*. That
//! case is not exotic; it is what a `--precise-stamps` flag is, and the only
//! way to spell it was to pair two `_if` calls with opposite polarities:
//!
//! ```ignore
//! // Gone, and this is why: two nodes, and one flipped `!` silently
//! // double-stamps the stage — the second write overwriting the first, with
//! // no error and a plausible-looking report.
//! s.stamp_if::<ingest>(!precise).stamp_precise_if::<ingest>(precise)
//! // Now:
//! s.stamp_as::<ingest>(Stamping::precise_if(precise))
//! ```
//!
//! So **the `_if` stamps are removed**, not merely superseded: a mode is one
//! argument, and [`Stamping::Off`] is how a stamp is turned off. `stamp()` and
//! `stamp_precise()` remain as the shorthands they are — `stamp()` is
//! `stamp_as(Stamping::Cycle)` — and because they are the spellings `nitro!`
//! dispatches through. What was `stamp_if(e)` is `stamp_as(Stamping::on_if(e))`.
//!
//! [`LatencyReportOps::latency_report_if`] is deliberately **not** part of that
//! removal. It is not a clock choice: [`ReportOutput::Silent`] still wires the
//! sink and accumulates, and only declines to print, whereas
//! `latency_report_if(false, ..)` wires a sink that never ticks and aggregates
//! nothing. No [`Stamping`]-shaped argument expresses that, so the toggle stays
//! a method.
//!
//! # Stamping several stages in one node
//!
//! A stamp op writes 8 bytes and clones the whole payload to do it — each node
//! owns its output slot, so the clone is the engine's model, not this module's
//! choice. Adjacent stamps therefore cost one clone *each*, and on a
//! burst-shaped stream a clone is a `Vec` allocation.
//!
//! [`stamp_all`](LatencyStreamOps::stamp_all) takes a **tuple of stages** and
//! writes them from one node: `N` stamps, one clone. It is not a shortcut with
//! different semantics — under [`Stamping::Precise`] it takes a *fresh* clock
//! read per stage, exactly as `N` chained `stamp_precise` nodes would, and
//! under [`Stamping::Cycle`] all `N` share the cycle snap, exactly as `N`
//! chained `stamp` nodes would. On a burst it reads the clock once per *stage*
//! and applies it to every value, because a burst is one instant.
//!
//! # Toggling
//!
//! [`Stamping::Off`] returns the upstream unchanged — no node inserted, zero
//! runtime cost. It is the whole toggle story for stamping: a config carries
//! one [`Stamping`] value and every stamp takes it as an argument, so turning
//! capture off is a value change rather than a wiring change.
//! [`Stamping::on_if`] and [`Stamping::precise_if`] build one from the two
//! flags a config usually has.
//!
//! Aggregation toggles separately, through
//! [`latency_report_if`](LatencyReportOps::latency_report_if) — see above for
//! why that one is not a [`Stamping`].
//!
//! # Tiers
//!
//! The four named stamps ([`stamp`](LatencyStreamOps::stamp),
//! [`stamp_precise`](LatencyStreamOps::stamp_precise) and their burst twins)
//! work in **all three** `nitro!` expansions — `interpreted()`, `compiled()`
//! and `nested()`.
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
//! [`stamp_as`](LatencyStreamOps::stamp_as) and
//! [`stamp_all`](LatencyStreamOps::stamp_all) are **fluent-only**, and that is
//! structural rather than a gap: both choose their node at *wiring* time from a
//! runtime value, and a `nitro!` graph's topology is fixed at expansion. Spell
//! the named stamps inside a `nitro!` block — the compiled tier fuses across
//! node boundaries anyway, which is the same win `stamp_all` buys the
//! interpreted one.
//!
//! [`latency_report`](LatencyReportOps::latency_report) stays
//! **interpreted-only**, also structural: the sink's whole value is the
//! [`LatencyHandle`] it hands back, and `compiled()` is outputs-only by design
//! — a closed box that returns its declared output values and nothing else.
//! There is no way for the handle to escape it, so a compiled `latency_report`
//! could only ever print at teardown, never be read. Deviation register **C7**.
//!
//! # Burst-shaped forms
//!
//! Adapters emit `Stream<Burst<T>>`, and [`collapse`](crate::ops::Collapse)
//! — the one-step bridge to the scalar combinators — keeps only the burst's
//! **last** value. On an ingest path carrying events rather than a
//! latest-wins signal that is silent data loss, and it only appears once a
//! producer outruns the graph cycle. So every op here has a burst-shaped form:
//!
//! - [`LatencyBurstStreamOps`] mirrors the scalar stamps under `_each` names —
//!   the clock is read **once per burst** (per stage), since a burst is one
//!   instant and a per-value read would invent differences that do not exist.
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
//! # Reading the numbers out
//!
//! [`latency_report`](LatencyReportOps::latency_report) returns a
//! [`LatencyHandle`], not a bare `Rc<RefCell<..>>`. Beyond hiding the sharing
//! mechanism it carries the two things a cumulative accumulator cannot do on
//! its own:
//!
//! - [`snapshot`](LatencyHandle::snapshot) / [`reset`](LatencyHandle::reset) —
//!   without a reset, one outlier pins the p99 for the life of the process.
//! - [`windows`](LatencyHandle::windows) — the same read wired as a
//!   `Stream<LatencySnapshot>`, one per period, so latency can drive gauges,
//!   alerts and downstream ops instead of only a teardown `print`.
//!
//! Where the teardown summary goes is [`ReportOutput`] — stdout, the `log`
//! crate, or nowhere.

use std::cell::{Cell, Ref, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;

use crate::Burst;
use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::op::{Activation, Ctx, Op, Tick};
use wingfoil_derive::op;

// The pure data layer is engine-agnostic and lives in `runtime::latency`.
pub use crate::runtime::latency::{
    HISTOGRAM_BUCKETS, HasLatency, HopStats, Latency, LatencySnapshot, LatencyStats, Stage,
    StageStats, TOTAL, Traced, format_latency_report, latency_stages, record_stage_deltas,
};

// ---------------------------------------------------------------------------
// Stamping — which clock, or none
// ---------------------------------------------------------------------------

/// Which clock a stamp reads — or whether it is wired at all.
///
/// The single argument that replaced the `stamp` / `stamp_precise` /
/// `stamp_if` / `stamp_precise_if` cross product — the `_if` half of which is
/// now gone. See the [module
/// docs](self#one-mode-argument-not-a-method-per-combination) for why that
/// matters beyond tidiness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stamping {
    /// Insert no node: the stage is left unstamped and the stream is returned
    /// unchanged. Zero runtime cost, and the report will say the hop was
    /// [`unstamped`](StageStats::unstamped) rather than fast.
    Off,
    /// Read [`Ctx::wall_time`](crate::op::Ctx::wall_time): the cycle-start
    /// snap, one `u64` load, shared by every stage stamped in the same engine
    /// cycle. The right choice for hops that cross a cycle or a process.
    #[default]
    Cycle,
    /// Read [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise): a
    /// fresh TSC read per stage (~5–10 ns), so stages inside one engine cycle
    /// get distinct timestamps. The right choice for in-process hops, which
    /// [`Cycle`](Self::Cycle) cannot measure at all.
    Precise,
}

impl Stamping {
    /// From the two flags a config usually carries: whether to instrument at
    /// all, and whether to pay for intra-cycle resolution.
    pub const fn new(enabled: bool, precise: bool) -> Self {
        match (enabled, precise) {
            (false, _) => Self::Off,
            (true, false) => Self::Cycle,
            (true, true) => Self::Precise,
        }
    }

    /// [`Precise`](Self::Precise) or [`Cycle`](Self::Cycle) — instrumented
    /// either way. The shape a `--precise-stamps` flag wants.
    pub const fn precise_if(precise: bool) -> Self {
        Self::new(true, precise)
    }

    /// [`Cycle`](Self::Cycle) or [`Off`](Self::Off). The shape an
    /// `--instrument` flag wants.
    pub const fn on_if(enabled: bool) -> Self {
        Self::new(enabled, false)
    }

    /// Whether this mode wires a node.
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

// ---------------------------------------------------------------------------
// StageSet — one stage, or several written by one node
// ---------------------------------------------------------------------------

/// A tuple of [`Stage`]s, stamped in tuple order by a single node.
///
/// Implemented for tuples of 1 to 8 stages. The single-stage form is `(S,)` —
/// note the trailing comma — though [`stamp_as`](LatencyStreamOps::stamp_as)
/// is the ergonomic spelling for one stage.
///
/// Deliberately not implemented blanket-wise for `S: Stage<L>`: [`Stage`] is
/// public, so a downstream crate could implement it *for* a tuple, and rustc
/// must assume it might — the blanket impl would collide with the tuple impls
/// under coherence.
pub trait StageSet<L: Latency> {
    /// How many stages this set writes.
    const LEN: usize;

    /// Stamp one record, calling `now` once per stage — so a fresh-clock `now`
    /// separates the stages and a cached one does not, matching what the same
    /// stages wired as separate nodes would produce.
    fn stamp_one<F: FnMut() -> u64>(latency: &mut L, now: &mut F);

    /// Stamp a whole same-instant group, calling `now` once **per stage** and
    /// applying each read to every value.
    ///
    /// The loop nesting is the contract: a burst is one instant, so reading
    /// the clock per value would invent differences that do not exist, while
    /// reading it once for the whole call would collapse stages a precise
    /// stamp is meant to separate.
    fn stamp_many<P, F>(values: &mut [P], now: &mut F)
    where
        P: HasLatency<L = L>,
        F: FnMut() -> u64;
}

macro_rules! count_stages {
    () => { 0usize };
    ($head:ident $(, $tail:ident)*) => { 1usize + count_stages!($($tail),*) };
}

macro_rules! impl_stage_set {
    ($($name:ident),+) => {
        impl<L: Latency, $($name: Stage<L>),+> StageSet<L> for ($($name,)+) {
            const LEN: usize = count_stages!($($name),+);

            #[inline]
            fn stamp_one<Func: FnMut() -> u64>(latency: &mut L, now: &mut Func) {
                $( <$name as Stage<L>>::stamp(latency, now()); )+
            }

            #[inline]
            fn stamp_many<P, Func>(values: &mut [P], now: &mut Func)
            where
                P: HasLatency<L = L>,
                Func: FnMut() -> u64,
            {
                $(
                    let t = now();
                    for value in values.iter_mut() {
                        <$name as Stage<L>>::stamp(value.latency_mut(), t);
                    }
                )+
            }
        }
    };
}

impl_stage_set!(A);
impl_stage_set!(A, B);
impl_stage_set!(A, B, C);
impl_stage_set!(A, B, C, D);
impl_stage_set!(A, B, C, D, E);
impl_stage_set!(A, B, C, D, E, F);
impl_stage_set!(A, B, C, D, E, F, G);
impl_stage_set!(A, B, C, D, E, F, G, H);

/// Stamp one record's stage set from `ctx`, taking the clock reads the mode
/// calls for. `precise` is the [`Stamping::Precise`] flag; [`Stamping::Off`]
/// never reaches here (no node is wired).
#[inline]
fn stamp_one_from_ctx<P, S>(value: &mut P, precise: bool, ctx: &Ctx<'_>)
where
    P: HasLatency,
    S: StageSet<P::L>,
{
    if precise {
        let mut now = || u64::from(ctx.wall_time_precise());
        S::stamp_one(value.latency_mut(), &mut now);
    } else {
        let snap = u64::from(ctx.wall_time());
        let mut now = || snap;
        S::stamp_one(value.latency_mut(), &mut now);
    }
}

/// [`stamp_one_from_ctx`] for a same-instant group.
#[inline]
fn stamp_many_from_ctx<P, S>(values: &mut [P], precise: bool, ctx: &Ctx<'_>)
where
    P: HasLatency,
    S: StageSet<P::L>,
{
    if precise {
        let mut now = || u64::from(ctx.wall_time_precise());
        S::stamp_many(values, &mut now);
    } else {
        let snap = u64::from(ctx.wall_time());
        let mut now = || snap;
        S::stamp_many(values, &mut now);
    }
}

// ---------------------------------------------------------------------------
// Stamp / StampPrecise — pass-through ops that stamp one stage
// ---------------------------------------------------------------------------

/// Op: forward the payload unchanged while stamping
/// [`Ctx::wall_time`](crate::op::Ctx::wall_time) (cycle-start snap) into a
/// single named stage `S` of the embedded [`Latency`] record. One `u64` store
/// per tick, no allocation.
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
/// distinct timestamps.
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

// ---------------------------------------------------------------------------
// StampAll — a whole stage set from one node
// ---------------------------------------------------------------------------

/// Op: forward the payload unchanged while stamping **every** stage in the set
/// `S`, from a single node — `N` stamps for one clone rather than `N`.
///
/// `Cfg` is the [`Stamping::Precise`] flag: true takes a fresh clock read per
/// stage, false shares the cycle snap across all of them, which is exactly
/// what the same stages wired as separate nodes would do.
///
/// Fluent-only (no `#[op]` forwarder): the stage *set* and the mode are both
/// wiring-time choices, and a `nitro!` graph's topology is fixed at expansion
/// — spell the named stamps there, where the compiled tier fuses across node
/// boundaries anyway.
pub struct StampAll<P, S>(PhantomData<fn() -> (P, S)>);

impl<P, S> Op for StampAll<P, S>
where
    P: Clone + Default + HasLatency + 'static,
    S: StageSet<P::L> + 'static,
{
    type Cfg = bool;
    type State = ();
    type In<'a> = (&'a P,);
    type Out = P;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut bool, _state: &mut (), input: (&P,), ctx: &mut Ctx<'_>) -> Result<Tick<P>> {
        let mut value = input.0.clone();
        stamp_one_from_ctx::<P, S>(&mut value, *cfg, ctx);
        Ok(Tick::Value(value))
    }
}

/// The burst-shaped [`StampAll`]: every stage in the set, every value in the
/// burst, one clock read per stage.
pub struct StampAllEach<P, S>(PhantomData<fn() -> (P, S)>);

impl<P, S> Op for StampAllEach<P, S>
where
    P: Clone + Default + HasLatency + 'static,
    S: StageSet<P::L> + 'static,
{
    type Cfg = bool;
    type State = ();
    type In<'a> = (&'a Burst<P>,);
    type Out = Burst<P>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut bool,
        _state: &mut (),
        input: (&Burst<P>,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<P>>> {
        let mut out = input.0.clone();
        stamp_many_from_ctx::<P, S>(&mut out, *cfg, ctx);
        Ok(Tick::Value(out))
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
    /// before forwarding. Shorthand for
    /// [`stamp_as`](Self::stamp_as)`(Stamping::Cycle)`.
    #[must_use]
    fn stamp<S: Stage<P::L> + 'static>(&self) -> Stream<P>;

    /// Like [`stamp`](Self::stamp) but uses
    /// [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise) for
    /// intra-cycle resolution. Shorthand for
    /// [`stamp_as`](Self::stamp_as)`(Stamping::Precise)`.
    #[must_use]
    fn stamp_precise<S: Stage<P::L> + 'static>(&self) -> Stream<P>;

    /// Stamp stage `S` under a [`Stamping`] mode chosen at runtime — the
    /// general form the two named stamps above are shorthands for, and the one
    /// to reach for whenever the clock is a config decision.
    ///
    /// [`Stamping::Off`] is how a stage is turned off: it returns `self`
    /// unchanged, no node inserted, zero runtime cost.
    #[must_use]
    fn stamp_as<S: Stage<P::L> + 'static>(&self, mode: Stamping) -> Stream<P>;

    /// Stamp a whole **tuple of stages** from one node, in tuple order.
    ///
    /// Semantically identical to chaining the stages one node each — under
    /// [`Stamping::Precise`] each stage still gets its own clock read — but it
    /// clones the payload once instead of once per stage. Prefer it wherever
    /// consecutive stamps sit together in a chain.
    ///
    /// ```ignore
    /// s.stamp_all::<(round_trip::ws_recv, round_trip::ws_publish)>(mode)
    /// ```
    #[must_use]
    fn stamp_all<S: StageSet<P::L> + 'static>(&self, mode: Stamping) -> Stream<P>;
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

    fn stamp_as<S: Stage<P::L> + 'static>(&self, mode: Stamping) -> Stream<P> {
        match mode {
            Stamping::Off => self.clone(),
            Stamping::Cycle => self.stamp::<S>(),
            Stamping::Precise => self.stamp_precise::<S>(),
        }
    }

    fn stamp_all<S: StageSet<P::L> + 'static>(&self, mode: Stamping) -> Stream<P> {
        if !mode.is_on() {
            return self.clone();
        }
        let precise = mode == Stamping::Precise;
        self.wire(move |b, h| {
            b.register_op1(
                h,
                "stamp_all",
                Activation::NONE,
                precise,
                || (),
                move |cfg: &mut bool, state: &mut (), value: &P, ctx: &mut Ctx<'_>| {
                    StampAll::<P, S>::cycle(cfg, state, (value,), ctx)
                },
            )
        })
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
// The clock is read **once per burst per stage**, not once per value: a burst
// is by definition one instant's worth of values, so a per-value read would
// invent differences that do not exist. A precise stamp still gives distinct
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

    /// Stamp stage `S` on every value in each burst, from a fresh TSC read.
    #[must_use]
    fn stamp_precise_each<S: Stage<P::L> + 'static>(&self) -> Stream<Burst<P>>;

    /// Stamp stage `S` on every value in each burst under a [`Stamping`] mode
    /// chosen at runtime — the burst-shaped twin of
    /// [`stamp_as`](LatencyStreamOps::stamp_as), and like it the general form
    /// the two named stamps above are shorthands for. [`Stamping::Off`] returns
    /// `self` unchanged, no node inserted.
    #[must_use]
    fn stamp_each_as<S: Stage<P::L> + 'static>(&self, mode: Stamping) -> Stream<Burst<P>>;

    /// Stamp a whole tuple of stages on every value in each burst, from one
    /// node — the burst-shaped twin of
    /// [`stamp_all`](LatencyStreamOps::stamp_all), and the bigger win of the
    /// two: a burst clone is a `Vec` allocation, so fusing `N` stamps saves
    /// `N - 1` of them per cycle.
    #[must_use]
    fn stamp_each_all<S: StageSet<P::L> + 'static>(&self, mode: Stamping) -> Stream<Burst<P>>;
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

    fn stamp_each_as<S: Stage<P::L> + 'static>(&self, mode: Stamping) -> Stream<Burst<P>> {
        match mode {
            Stamping::Off => self.clone(),
            Stamping::Cycle => self.stamp_each::<S>(),
            Stamping::Precise => self.stamp_precise_each::<S>(),
        }
    }

    fn stamp_each_all<S: StageSet<P::L> + 'static>(&self, mode: Stamping) -> Stream<Burst<P>> {
        if !mode.is_on() {
            return self.clone();
        }
        let precise = mode == Stamping::Precise;
        self.wire(move |b, h| {
            b.register_op1(
                h,
                "stamp_each_all",
                Activation::NONE,
                precise,
                || (),
                move |cfg: &mut bool, state: &mut (), value: &Burst<P>, ctx: &mut Ctx<'_>| {
                    StampAllEach::<P, S>::cycle(cfg, state, (value,), ctx)
                },
            )
        })
    }
}

// ---------------------------------------------------------------------------
// LatencyReport — sink op aggregating per-stage delta statistics
// ---------------------------------------------------------------------------

/// Where a [`LatencyReport`] sink writes its teardown summary.
///
/// Replaces a bare `print_on_teardown: bool` — which was both unreadable at
/// the call site (`latency_report(true)`) and hard-wired to `stdout`, in a
/// library whose users routinely reserve stdout for structured output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReportOutput {
    /// Print nothing; read the numbers through the [`LatencyHandle`].
    #[default]
    Silent,
    /// Print the summary to standard output at teardown.
    Stdout,
    /// Emit the summary through `log::info!` at teardown, so it lands wherever
    /// the process's logging is configured to go.
    Log,
}

impl ReportOutput {
    /// Write `report` wherever this variant says.
    ///
    /// Public so a hand-rolled sink — the Python binding's runtime-named
    /// aggregator, or a user's own — routes its summary the same way rather
    /// than reimplementing the choice.
    pub fn emit(self, report: &str) {
        match self {
            Self::Silent => {}
            Self::Stdout => print!("{report}"),
            Self::Log => log::info!("{report}"),
        }
    }
}

/// Construction config for a [`LatencyReport`] sink: the shared stats
/// accumulator plus where to write the summary at [`stop`](Op::stop).
pub struct LatencyReportCfg<L: Latency> {
    /// The accumulator the sink folds each observation into. Shared, so the
    /// caller can read the numbers out during or after the run.
    pub stats: Rc<RefCell<LatencyStats<L>>>,
    /// Where the per-stage summary goes when the run stops.
    pub output: ReportOutput,
}

/// Sink op consuming a stream of `P: HasLatency`, accumulating per-stage delta
/// statistics into a shared [`LatencyStats`]. At [`stop`](Op::stop) it writes
/// the summary to its [`ReportOutput`].
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
        cfg.output.emit(&cfg.stats.borrow().format_report());
        Ok(())
    }
}

/// The handle a [`latency_report`](LatencyReportOps::latency_report) hands
/// back: a live, shared view of the statistics its sink accumulates.
///
/// A newtype rather than the bare `Rc<RefCell<LatencyStats<L>>>` it wraps, for
/// three reasons that are all about what the old shape *could not* do:
///
/// * **Reset.** Statistics are cumulative for the life of a run, so one
///   outlier pins the p99 forever — a gauge fed from an un-resettable
///   accumulator records the worst moment the process ever had, not the state
///   it is in. [`reset`](Self::reset) and [`windows`](Self::windows) fix that.
/// * **Read-out without indexing.** [`snapshot`](Self::snapshot) and
///   [`hops`](Self::hops) return labelled [`HopStats`], so no caller has to
///   reproduce the `stages[1..N]` convention (and none has to know that
///   `stages[`[`TOTAL`]`]` is the end-to-end row).
/// * **Room to change.** The sharing mechanism is no longer in the signature.
pub struct LatencyHandle<L: Latency> {
    inner: Rc<RefCell<LatencyStats<L>>>,
    windows_wired: Rc<Cell<bool>>,
}

impl<L: Latency> Clone for LatencyHandle<L> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            windows_wired: self.windows_wired.clone(),
        }
    }
}

impl<L: Latency> Default for LatencyHandle<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: Latency> LatencyHandle<L> {
    /// A handle over a fresh, empty accumulator.
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(LatencyStats::new())),
            windows_wired: Rc::new(Cell::new(false)),
        }
    }

    /// The shared accumulator, for the wiring layer.
    pub(crate) fn shared(&self) -> Rc<RefCell<LatencyStats<L>>> {
        self.inner.clone()
    }

    /// Borrow the accumulator directly, for the statistics the labelled
    /// read-outs below do not expose — the raw histogram, say.
    ///
    /// Panics if held across a cycle in which the sink observes a value, so
    /// keep it to a single expression.
    pub fn borrow(&self) -> Ref<'_, LatencyStats<L>> {
        self.inner.borrow()
    }

    /// A point-in-time read of every hop plus the end-to-end total.
    pub fn snapshot(&self) -> LatencySnapshot {
        self.inner.borrow().snapshot()
    }

    /// Every hop, in stamp order, as labelled summaries.
    pub fn hops(&self) -> Vec<HopStats> {
        self.inner.borrow().hops()
    }

    /// The end-to-end summary: first declared stage to last.
    pub fn total(&self) -> HopStats {
        self.inner.borrow().total()
    }

    /// The multi-line report, exactly as [`ReportOutput`] would write it.
    pub fn report(&self) -> String {
        self.inner.borrow().format_report()
    }

    /// Drop every sample and tally, keeping the stage layout.
    pub fn reset(&self) {
        self.inner.borrow_mut().reset();
    }

    /// [`snapshot`](Self::snapshot) then [`reset`](Self::reset), atomically
    /// with respect to the graph: the returned snapshot describes exactly the
    /// period since the previous `take`, and no observation falls between the
    /// two halves.
    pub fn take(&self) -> LatencySnapshot {
        let mut stats = self.inner.borrow_mut();
        let snapshot = stats.snapshot();
        stats.reset();
        snapshot
    }

    /// A stream of **per-window** snapshots, one per `period`, each describing
    /// only the period since the last.
    ///
    /// This is how latency leaves the engine as data rather than as a teardown
    /// `print`: feed it to gauges, alerts, or any downstream op. It is windowed
    /// rather than cumulative because a cumulative p99 never recovers from an
    /// outlier — it is a record, not a reading. Note that it *resets* the
    /// shared accumulator, so the teardown report of a windowed handle covers
    /// only the final window; for cumulative readings on a timer, map a ticker
    /// over [`snapshot`](Self::snapshot) instead.
    ///
    /// **Match `period` to whatever consumes the stream.** The read is
    /// destructive, which inverts the usual "sample at least as often as you
    /// scrape" instinct: a window that opens and closes between two reads is
    /// reset before anything observes it, so windowing faster than the consumer
    /// does not buy resolution — it discards every window but the last one
    /// before each read, and a spike in a discarded window is invisible. A
    /// consumer that polls *this stream* (a gauge, a downstream op) reads every
    /// window by construction; one that polls the exporter behind it
    /// (Prometheus scraping a gauge) does not, and needs `period` equal to its
    /// own interval. `trading_e2e`'s `LATENCY_WINDOW` is that case.
    ///
    /// **At most one `windows` stream per accumulator** — the destructive read
    /// means a second one would steal samples from the first, splitting counts
    /// arbitrarily between them, so wiring it panics. To fan one window stream
    /// out to several consumers, wire `windows` once and branch the returned
    /// stream; for additional cadences, map tickers over
    /// [`snapshot`](Self::snapshot) (cumulative, non-destructive) instead.
    pub fn windows(&self, g: &GraphBuilder, period: Duration) -> Stream<LatencySnapshot> {
        assert!(
            !self.windows_wired.replace(true),
            "windows() already wired for this LatencyHandle: its read is \
             destructive (take()), so a second windows stream would silently \
             steal samples from the first. Branch the existing stream, or map \
             a ticker over snapshot() for a cumulative reading."
        );
        let handle = self.clone();
        g.ticker(period).map(move |_: &()| handle.take())
    }
}

/// Extension methods to install a [`LatencyReport`] sink. Returns the sink
/// stream plus the [`LatencyHandle`], so the caller can inspect the numbers
/// during or after the run.
pub trait LatencyReportOps<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    /// Install a [`LatencyReport`] sink writing its teardown summary to
    /// `on_teardown`. Returns `(sink_stream, handle)`.
    fn latency_report(&self, on_teardown: ReportOutput) -> (Stream<()>, LatencyHandle<P::L>);

    /// Conditional variant. When `enabled` is false, installs a sink that
    /// never ticks and returns an empty handle (counts stay at zero) —
    /// letting a single config flag toggle aggregation without wiring edits.
    fn latency_report_if(
        &self,
        enabled: bool,
        on_teardown: ReportOutput,
    ) -> (Stream<()>, LatencyHandle<P::L>);
}

impl<P> LatencyReportOps<P> for Stream<P>
where
    P: Clone + Default + HasLatency + 'static,
{
    fn latency_report(&self, on_teardown: ReportOutput) -> (Stream<()>, LatencyHandle<P::L>) {
        let handle = LatencyHandle::new();
        let stats = handle.shared();
        let stream = self.wire(move |b, h| b.latency_report(h, on_teardown, stats));
        (stream, handle)
    }

    fn latency_report_if(
        &self,
        enabled: bool,
        on_teardown: ReportOutput,
    ) -> (Stream<()>, LatencyHandle<P::L>) {
        if enabled {
            self.latency_report(on_teardown)
        } else {
            // A source that never ticks: nothing is observed, stats stay empty.
            let stream = self.wire(|b, _h| b.never());
            (stream, LatencyHandle::new())
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
        cfg.output.emit(&cfg.stats.borrow().format_report());
        Ok(())
    }
}

/// The burst-shaped report: **the same trait and the same method names**,
/// selected by the receiver's shape. `stream.latency_report(out)` means
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
    fn latency_report(&self, on_teardown: ReportOutput) -> (Stream<()>, LatencyHandle<P::L>) {
        let handle = LatencyHandle::new();
        let stats = handle.shared();
        let stream = self.wire(move |b, h| b.latency_report_each(h, on_teardown, stats));
        (stream, handle)
    }

    fn latency_report_if(
        &self,
        enabled: bool,
        on_teardown: ReportOutput,
    ) -> (Stream<()>, LatencyHandle<P::L>) {
        if enabled {
            self.latency_report(on_teardown)
        } else {
            // A source that never ticks: nothing is observed, stats stay empty.
            let stream = self.wire(|b, _h| b.never());
            (stream, LatencyHandle::new())
        }
    }
}
