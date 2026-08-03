//! An island's ops must read the **outer cycle's** wall snap.
//!
//! `Ctx::nested` used to call [`NanoTime::now`] itself, and the `nitro!`
//! nested expansion builds one `Ctx` **per inner node, per activation** — so
//! every node of every island paid a TSC read (~24ns; `benches/nanotime.rs`
//! prices exactly that call), and an island's ops disagreed with the rest of
//! the graph, and with each other, about when "now" was.
//!
//! The unit tests beside `Ctx::nested` pin the constructor: given a
//! `wall_time`, it stores it. They cannot see the defect, because the defect
//! was never in the constructor — it was in **what the macro passes**. A
//! nested expansion that called `Ctx::nested(__now, NanoTime::now(), ..)`
//! per node would satisfy every assertion there. These tests close that gap
//! by going through `nitro!` itself, and they are the ones that fail if the
//! per-node clock read comes back:
//!
//! - [`island_ops_see_the_outer_cycles_wall_snap`] — the island agrees with
//!   the *rest of the graph*: the same wiring as `wire()` (ordinary
//!   interpreted nodes, whose `Ctx` comes from the kernel) reports the same
//!   stamps as `nested()`, cycle for cycle.
//! - [`island_nodes_agree_within_one_activation`] — the island agrees with
//!   *itself*: two inner nodes at different depths in one activation observe
//!   one instant, so their difference is exactly zero.
//!
//! Both are exact-equality assertions with no timing tolerance, which is what
//! makes them deterministic: sharing one snap is the only way to hit them.

use std::time::Duration;

use wingfoil::anyhow::Result;
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_nanos(100);
const CYCLES: u32 = 4;

// ---------------------------------------------------------------------------
// Two user ops that surface `Ctx::wall_time` as a stream value. The catalog
// has no such op reachable from `nitro!` — `latency::Stamp` buries the value
// inside a payload's `Latency` record and has no table row — so the property
// is not otherwise observable from inside an island.
// ---------------------------------------------------------------------------

/// Emit the cycle's wall snap as the value.
pub struct WallStamp;

impl Op for WallStamp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a u64,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        _input: (&u64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        Ok(Tick::Value(u64::from(ctx.wall_time())))
    }
}

/// Emit this node's wall snap *minus* the upstream node's. Zero exactly when
/// the two nodes saw the same instant; positive if each read its own clock
/// (the later read cannot precede the earlier one).
pub struct WallGap;

impl Op for WallGap {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a u64,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&u64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        Ok(Tick::Value(
            u64::from(ctx.wall_time()).saturating_sub(*input.0),
        ))
    }
}

// ---------------------------------------------------------------------------
// The forwarders `nitro!`'s generic fallback calls by naming convention,
// hand-written as in `tests/custom_op.rs` (the in-crate `#[op]` attribute is
// not usable out-of-crate — it also emits an inherent `impl Builder`).
// ---------------------------------------------------------------------------

pub const __WF_OP_WALL_STAMP_ACTIVATION: Activation = WallStamp::ACTIVATION;
pub const __WF_OP_WALL_STAMP_PASSIVE: u32 = 0;
pub const __WF_OP_WALL_GAP_ACTIVATION: Activation = WallGap::ACTIVATION;
pub const __WF_OP_WALL_GAP_PASSIVE: u32 = 0;

pub fn __wf_op_wall_stamp_cycle(
    cfg: &mut <WallStamp as Op>::Cfg,
    state: &mut <WallStamp as Op>::State,
    input: ((&u64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<WallStamp as Op>::Out>> {
    <WallStamp as Op>::cycle(cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_wall_gap_cycle(
    cfg: &mut <WallGap as Op>::Cfg,
    state: &mut <WallGap as Op>::State,
    input: ((&u64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<WallGap as Op>::Out>> {
    <WallGap as Op>::cycle(cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_wall_stamp_start<C, S>(
    _cfg: &mut C,
    _state: &mut S,
    _ctx: &mut Ctx<'_>,
) -> Result<()> {
    Ok(())
}

pub fn __wf_op_wall_gap_start<C, S>(
    _cfg: &mut C,
    _state: &mut S,
    _ctx: &mut Ctx<'_>,
) -> Result<()> {
    Ok(())
}

pub fn __wf_op_wall_stamp_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_wall_stamp_seed_value<P>(_cfg: &P) -> u64 {
    0
}
pub fn __wf_op_wall_gap_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_wall_gap_seed_value<P>(_cfg: &P) -> u64 {
    0
}

pub fn __wf_op_wall_stamp_stop(
    cfg: &mut <WallStamp as Op>::Cfg,
    state: &mut <WallStamp as Op>::State,
    input: ((&u64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <WallStamp as Op>::stop(cfg, state, ctx)
}

pub fn __wf_op_wall_stamp_teardown(
    cfg: &mut <WallStamp as Op>::Cfg,
    state: &mut <WallStamp as Op>::State,
    input: ((&u64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <WallStamp as Op>::teardown(cfg, state, ctx)
}

pub fn __wf_op_wall_gap_stop(
    cfg: &mut <WallGap as Op>::Cfg,
    state: &mut <WallGap as Op>::State,
    input: ((&u64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <WallGap as Op>::stop(cfg, state, ctx)
}

pub fn __wf_op_wall_gap_teardown(
    cfg: &mut <WallGap as Op>::Cfg,
    state: &mut <WallGap as Op>::State,
    input: ((&u64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <WallGap as Op>::teardown(cfg, state, ctx)
}

// ---------------------------------------------------------------------------
// The fluent methods, so `wire()` sees the same vocabulary as the nested
// expansion — one-liners over the single-input registration primitive, as in
// `tests/custom_op.rs`.
// ---------------------------------------------------------------------------

trait WallOps {
    fn wall_stamp(&self) -> Stream<u64>;
    fn wall_gap(&self) -> Stream<u64>;
}

impl WallOps for Stream<u64> {
    fn wall_stamp(&self) -> Stream<u64> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "wall_stamp",
                WallStamp::ACTIVATION,
                (),
                || (),
                |c, s, a, ctx| <WallStamp as Op>::cycle(c, s, (a,), ctx),
            )
        })
    }

    fn wall_gap(&self) -> Stream<u64> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "wall_gap",
                WallGap::ACTIVATION,
                (),
                || (),
                |c, s, a, ctx| <WallGap as Op>::cycle(c, s, (a,), ctx),
            )
        })
    }
}

// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn stamp(_g: &GraphBuilder, src: &Stream<u64>) -> Stream<u64> {
        let out = src.wall_stamp();
        out
    }
}

wingfoil::nitro! {
    fn stamp_gap(_g: &GraphBuilder, src: &Stream<u64>) -> Stream<u64> {
        let first = src.wall_stamp();
        let gap = first.wall_gap();
        gap
    }
}

/// The island agrees with the rest of the graph. `wire()` builds ordinary
/// interpreted nodes, whose `Ctx` reads the kernel's per-cycle snap;
/// `nested()` mounts the same wiring as a compiled island. Fed by one source,
/// under one kernel, they must report identical stamps on every cycle — the
/// property a per-node `NanoTime::now()` inside the island destroys, since
/// each island node would then stamp an instant strictly later than the
/// kernel's.
#[test]
fn island_ops_see_the_outer_cycles_wall_snap() {
    let g = GraphBuilder::new();
    let src = g.ticker(PERIOD).count();

    let flat = stamp::wire(&g, &src).accumulate();
    let island = stamp::nested(&g, &src).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(CYCLES)).unwrap();

    let flat_stamps = runner.value(&flat);
    assert_eq!(
        CYCLES as usize,
        flat_stamps.len(),
        "one stamp per cycle, or the graph did not run",
    );
    assert!(
        flat_stamps.iter().all(|&t| t > 0),
        "wall snaps must be real timestamps, else this test is vacuous: {flat_stamps:?}",
    );
    assert_eq!(
        flat_stamps,
        runner.value(&island),
        "island stamps must equal the interpreted ones — same cycle, same snap",
    );
}

/// The snap is cached **per cycle**, not per run. `Kernel::wall_time` resolves
/// lazily and memoises, so the reset in `begin_cycle` is what stops every cycle
/// of a run reporting one frozen instant — and a missing reset is invisible to
/// the agreement tests here, which would pass just as happily on a constant.
/// Both engines are checked: the interpreted nodes read the kernel's cache
/// directly, the island reads whatever the composite resolved for it.
#[test]
fn the_wall_snap_is_re_taken_each_cycle() {
    const LONG: u32 = 50;

    let g = GraphBuilder::new();
    let src = g.ticker(PERIOD).count();

    let flat = stamp::wire(&g, &src).accumulate();
    let island = stamp::nested(&g, &src).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(LONG)).unwrap();

    for (label, stamps) in [
        ("interpreted", runner.value(&flat)),
        ("island", runner.value(&island)),
    ] {
        assert_eq!(LONG as usize, stamps.len(), "{label}: one stamp per cycle");
        assert!(
            stamps.windows(2).all(|w| w[0] <= w[1]),
            "{label}: wall snaps must not go backwards: {stamps:?}",
        );
        assert!(
            stamps[0] < stamps[LONG as usize - 1],
            "{label}: the snap is frozen across the whole run — `begin_cycle` is not \
             clearing the cache: {stamps:?}",
        );
    }
}

/// The island agrees with itself. Two inner nodes at different depths run in
/// one activation, so the second's snap minus the first's is exactly zero.
/// With a clock read per `Ctx::nested` the second read happens strictly after
/// the first and the difference goes positive.
#[test]
fn island_nodes_agree_within_one_activation() {
    let g = GraphBuilder::new();
    let src = g.ticker(PERIOD).count();

    let flat = stamp_gap::wire(&g, &src).accumulate();
    let island = stamp_gap::nested(&g, &src).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(CYCLES)).unwrap();

    assert_eq!(
        vec![0u64; CYCLES as usize],
        runner.value(&flat),
        "two interpreted nodes in one cycle already observe one instant",
    );
    assert_eq!(
        vec![0u64; CYCLES as usize],
        runner.value(&island),
        "two nodes in one island activation must observe one instant",
    );
}
