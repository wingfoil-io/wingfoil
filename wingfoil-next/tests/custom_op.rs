//! **Generic-fallback proof**: ops with **no `OpKind` table row** flowing
//! through `graph!`'s three expansions — `interpreted()`, fully-monomorphized
//! `compiled()`, and `nested()` islands.
//!
//! The load-bearing trick under test: the macro sees only a method-name
//! *token* (`.delta()`), so it can never name the op's concrete type. Instead
//! it emits calls to naming-convention forwarder functions
//! (`__wf_op_delta_cycle` …) whose generic signatures let **rustc's type
//! inference** resolve the op type from the argument types at the expansion
//! site — including a state local declared as a bare `Default::default()`
//! whose type only exists as the associated projection `<Delta<T> as
//! Op>::State`. LLVM then monomorphizes the call exactly like a hand-written
//! table row.
//!
//! Five op shapes are exercised, all defined *in this test crate* (i.e. from
//! the library's point of view: user code):
//!
//! - [`Scale`] — plain config (`Cfg = f64`), no state;
//! - [`Delta<T>`] — **generic** op with inferred state (`State = Option<T>`);
//! - [`Apply<F>`] — **closure config** (`Cfg = F`), via the `_cycle_owned`
//!   forwarder (the same inference-deferral trick as `cycle_owned_cfg`);
//! - [`Spread`] — **two inputs**, no config: `.spread(&other)`. The `&other`
//!   argument is classified as a stream *edge* at expansion time (it names a
//!   stream bound in the graph), so the fallback wires `In = (&recv, &other)`
//!   — the `join` shape, both edges active;
//! - [`Combine<A, B, C, F>`] — **two inputs + closure config**
//!   (`.combine(&other, |a, b| ..)`) — the user-defined-join shape, generic
//!   in both input types and the closure;
//!
//! plus in-crate [`Distinct`](wingfoil_next::ops) — which has `#[op]` but **no
//! table row**, proving the whole `#[op]` catalog reaches `graph!` through the
//! fallback with zero per-op macro edits; and, at the end of the file, the
//! two formerly-"exotic" shapes as user ops — [`Snap`] (passive data edge,
//! sample's shape) and [`Ratchet`] (seeded accumulator, fold's shape).

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::anyhow::Result;
use wingfoil_next::op::{Activation, Ctx, Op, Tick};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

// ---------------------------------------------------------------------------
// The user ops: ordinary `Op` impls, exactly as they'd appear in an app crate.
// ---------------------------------------------------------------------------

/// Multiply each value by a constant factor. `Cfg = f64`, stateless.
pub struct Scale;

impl Op for Scale {
    type Cfg = f64;
    type State = ();
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut f64,
        _state: &mut (),
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(input.0 * *cfg))
    }
}

/// Successive difference — a **generic** user op: `State = Option<T>` must be
/// inferred through the forwarder call (the macro never writes the type).
pub struct Delta<T>(std::marker::PhantomData<T>);

impl<T> Op for Delta<T>
where
    T: Clone + std::ops::Sub<Output = T> + 'static,
{
    type Cfg = ();
    type State = Option<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let curr = input.0.clone();
        let out = match state.take() {
            Some(prev) => Tick::Value(curr.clone() - prev),
            None => Tick::Quiet,
        };
        *state = Some(curr);
        Ok(out)
    }
}

/// Apply a closure — the op whose **config is the closure** (`Cfg = F`), like
/// the built-in map. Exercises the `_cycle_owned` literal-closure path.
pub struct Apply<F>(std::marker::PhantomData<F>);

impl<F> Op for Apply<F>
where
    F: Fn(&f64) -> f64 + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut F,
        _state: &mut (),
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(cfg(input.0)))
    }
}

/// Difference of two streams: `a − b`. Two active inputs, no config, no
/// state — the minimal multi-input op.
pub struct Spread;

impl Op for Spread {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a f64, &'a f64);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&f64, &f64),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(input.0 - input.1))
    }
}

/// A user-defined `join`: combine two streams with a closure — generic in
/// both input types, the output, and the closure. The primary custom-op use
/// case: multiple inputs *and* a closure config, all resolved by inference.
pub struct Combine<A, B, C, F>(std::marker::PhantomData<(A, B, C, F)>);

impl<A, B, C, F> Op for Combine<A, B, C, F>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> C + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B);
    type Out = C;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A, &B), _ctx: &mut Ctx<'_>) -> Result<Tick<C>> {
        Ok(Tick::Value(cfg(input.0, input.1)))
    }
}

// ---------------------------------------------------------------------------
// The forwarders `graph!`'s generic fallback calls by naming convention —
// hand-written here as what a user-facing `#[op]` derive would generate.
// (The in-crate `#[op]` attribute already generates exactly these for the
// catalog ops; it isn't usable out-of-crate yet because it also emits an
// inherent `impl Builder`.)
// ---------------------------------------------------------------------------

pub const __WF_OP_SCALE_ACTIVATION: Activation = Scale::ACTIVATION;
pub const __WF_OP_SCALE_PASSIVE: u32 = 0;
pub const __WF_OP_DELTA_ACTIVATION: Activation = Activation::NONE;
pub const __WF_OP_DELTA_PASSIVE: u32 = 0;
pub const __WF_OP_APPLY_ACTIVATION: Activation = Activation::NONE;
pub const __WF_OP_APPLY_PASSIVE: u32 = 0;
pub const __WF_OP_SPREAD_ACTIVATION: Activation = Spread::ACTIVATION;
pub const __WF_OP_SPREAD_PASSIVE: u32 = 0;
pub const __WF_OP_COMBINE_ACTIVATION: Activation = Activation::NONE;
pub const __WF_OP_COMBINE_PASSIVE: u32 = 0;

// Cycle forwarders take the uniform `(value, tick)` pair per edge and adapt
// to the op's `In`; `_owned` variants take the literal-closure config by
// value plus the (empty) plain-config slot.

pub fn __wf_op_scale_cycle(
    cfg: &mut <Scale as Op>::Cfg,
    state: &mut <Scale as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Scale as Op>::Out>> {
    <Scale as Op>::cycle(cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_delta_cycle<T: Clone + std::ops::Sub<Output = T> + 'static>(
    cfg: &mut <Delta<T> as Op>::Cfg,
    state: &mut <Delta<T> as Op>::State,
    input: ((&T, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Delta<T> as Op>::Out>> {
    <Delta<T> as Op>::cycle(cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_apply_cycle_owned<F: Fn(&f64) -> f64 + 'static>(
    mut cfg: <Apply<F> as Op>::Cfg,
    _plain: &mut (),
    state: &mut <Apply<F> as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Apply<F> as Op>::Out>> {
    <Apply<F> as Op>::cycle(&mut cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_spread_cycle(
    cfg: &mut <Spread as Op>::Cfg,
    state: &mut <Spread as Op>::State,
    input: ((&f64, bool), (&f64, bool)),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Spread as Op>::Out>> {
    <Spread as Op>::cycle(cfg, state, (input.0.0, input.1.0), ctx)
}

pub fn __wf_op_combine_cycle_owned<A, B, C, F>(
    mut cfg: <Combine<A, B, C, F> as Op>::Cfg,
    _plain: &mut (),
    state: &mut <Combine<A, B, C, F> as Op>::State,
    input: ((&A, bool), (&B, bool)),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Combine<A, B, C, F> as Op>::Out>>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> C + 'static,
{
    <Combine<A, B, C, F> as Op>::cycle(&mut cfg, state, (input.0.0, input.1.0), ctx)
}

// Start forwarders: none of these ops override `Op::start`, so they are the
// fully-erased no-ops the `#[op]` derive would generate (op generics must
// not appear — they would dangle at the call site).

pub fn __wf_op_scale_start<C, S>(_cfg: &mut C, _state: &mut S, _ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}

pub fn __wf_op_delta_start<C, S>(_cfg: &mut C, _state: &mut S, _ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}

pub fn __wf_op_apply_start_owned<P, S>(
    _plain: &mut P,
    _state: &mut S,
    _ctx: &mut Ctx<'_>,
) -> Result<()> {
    Ok(())
}

pub fn __wf_op_spread_start<C, S>(_cfg: &mut C, _state: &mut S, _ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}

pub fn __wf_op_combine_start_owned<P, S>(
    _plain: &mut P,
    _state: &mut S,
    _ctx: &mut Ctx<'_>,
) -> Result<()> {
    Ok(())
}

// Seed forwarders: structural — generic only over the params the State/Out
// tokens mention, so nothing dangles. All defaults here (no fold-style
// config-derived seeds among these ops).

pub fn __wf_op_scale_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_scale_seed_value<P>(_cfg: &P) -> f64 {
    0.0
}
pub fn __wf_op_delta_seed_state<T, P>(_cfg: &P) -> Option<T> {
    None
}
pub fn __wf_op_delta_seed_value<T: Default, P>(_cfg: &P) -> T {
    T::default()
}
pub fn __wf_op_apply_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_apply_seed_value<P>(_cfg: &P) -> f64 {
    0.0
}
pub fn __wf_op_spread_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_spread_seed_value<P>(_cfg: &P) -> f64 {
    0.0
}
pub fn __wf_op_combine_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_combine_seed_value<C: Default, P>(_cfg: &P) -> C {
    C::default()
}

// Stop / teardown forwarders: each mirrors its op's cycle forwarder (same
// input parameter, present only to anchor generics/closures) and calls the
// hook — the trait default no-op here, as none of these ops override
// stop/teardown. The `graph!` tail calls one per node at the cleanup tail.
pub fn __wf_op_scale_stop(
    cfg: &mut <Scale as Op>::Cfg,
    state: &mut <Scale as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Scale as Op>::stop(cfg, state, ctx)
}
pub fn __wf_op_scale_teardown(
    cfg: &mut <Scale as Op>::Cfg,
    state: &mut <Scale as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Scale as Op>::teardown(cfg, state, ctx)
}
pub fn __wf_op_delta_stop<T: Clone + std::ops::Sub<Output = T> + 'static>(
    cfg: &mut <Delta<T> as Op>::Cfg,
    state: &mut <Delta<T> as Op>::State,
    input: ((&T, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Delta<T> as Op>::stop(cfg, state, ctx)
}
pub fn __wf_op_delta_teardown<T: Clone + std::ops::Sub<Output = T> + 'static>(
    cfg: &mut <Delta<T> as Op>::Cfg,
    state: &mut <Delta<T> as Op>::State,
    input: ((&T, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Delta<T> as Op>::teardown(cfg, state, ctx)
}
pub fn __wf_op_apply_stop_owned<F: Fn(&f64) -> f64 + 'static>(
    mut cfg: <Apply<F> as Op>::Cfg,
    _plain: &mut (),
    state: &mut <Apply<F> as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Apply<F> as Op>::stop(&mut cfg, state, ctx)
}
pub fn __wf_op_apply_teardown_owned<F: Fn(&f64) -> f64 + 'static>(
    mut cfg: <Apply<F> as Op>::Cfg,
    _plain: &mut (),
    state: &mut <Apply<F> as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Apply<F> as Op>::teardown(&mut cfg, state, ctx)
}
pub fn __wf_op_spread_stop(
    cfg: &mut <Spread as Op>::Cfg,
    state: &mut <Spread as Op>::State,
    input: ((&f64, bool), (&f64, bool)),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Spread as Op>::stop(cfg, state, ctx)
}
pub fn __wf_op_spread_teardown(
    cfg: &mut <Spread as Op>::Cfg,
    state: &mut <Spread as Op>::State,
    input: ((&f64, bool), (&f64, bool)),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Spread as Op>::teardown(cfg, state, ctx)
}
pub fn __wf_op_combine_stop_owned<A, B, C, F>(
    mut cfg: <Combine<A, B, C, F> as Op>::Cfg,
    _plain: &mut (),
    state: &mut <Combine<A, B, C, F> as Op>::State,
    input: ((&A, bool), (&B, bool)),
    ctx: &mut Ctx<'_>,
) -> Result<()>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> C + 'static,
{
    let _ = input;
    <Combine<A, B, C, F> as Op>::stop(&mut cfg, state, ctx)
}
pub fn __wf_op_combine_teardown_owned<A, B, C, F>(
    mut cfg: <Combine<A, B, C, F> as Op>::Cfg,
    _plain: &mut (),
    state: &mut <Combine<A, B, C, F> as Op>::State,
    input: ((&A, bool), (&B, bool)),
    ctx: &mut Ctx<'_>,
) -> Result<()>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> C + 'static,
{
    let _ = input;
    <Combine<A, B, C, F> as Op>::teardown(&mut cfg, state, ctx)
}

// ---------------------------------------------------------------------------
// The fluent methods, so `wire()`/`interpreted()` see the same vocabulary —
// one-liners over the (now public) single-input registration primitive.
// ---------------------------------------------------------------------------

trait CustomOps {
    fn scale(&self, factor: f64) -> Stream<f64>;
    fn delta(&self) -> Stream<f64>;
    fn apply<F: Fn(&f64) -> f64 + 'static>(&self, f: F) -> Stream<f64>;
    fn spread(&self, other: &Stream<f64>) -> Stream<f64>;
    fn combine<F: Fn(&f64, &f64) -> f64 + 'static>(&self, other: &Stream<f64>, f: F)
    -> Stream<f64>;
}

impl CustomOps for Stream<f64> {
    fn scale(&self, factor: f64) -> Stream<f64> {
        self.wire(|b, h| {
            b.register_op1(h, "scale", Scale::ACTIVATION, factor, (), |c, s, a, ctx| {
                <Scale as Op>::cycle(c, s, (a,), ctx)
            })
        })
    }

    fn delta(&self) -> Stream<f64> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "delta",
                <Delta<f64> as Op>::ACTIVATION,
                (),
                None,
                |c, s, a, ctx| <Delta<f64> as Op>::cycle(c, s, (a,), ctx),
            )
        })
    }

    fn apply<F: Fn(&f64) -> f64 + 'static>(&self, f: F) -> Stream<f64> {
        self.wire(|b, h| {
            b.register_op1(
                h,
                "apply",
                <Apply<F> as Op>::ACTIVATION,
                f,
                (),
                |c, s, a, ctx| <Apply<F> as Op>::cycle(c, s, (a,), ctx),
            )
        })
    }

    fn spread(&self, other: &Stream<f64>) -> Stream<f64> {
        let other = other.handle();
        self.wire(|builder, h| {
            builder.register_op2(
                h,
                other,
                "spread",
                Spread::ACTIVATION,
                (),
                (),
                |c, s, a, b, ctx| <Spread as Op>::cycle(c, s, (a, b), ctx),
            )
        })
    }

    fn combine<F: Fn(&f64, &f64) -> f64 + 'static>(
        &self,
        other: &Stream<f64>,
        f: F,
    ) -> Stream<f64> {
        let other = other.handle();
        self.wire(|builder, h| {
            builder.register_op2(
                h,
                other,
                "combine",
                <Combine<f64, f64, f64, F> as Op>::ACTIVATION,
                f,
                (),
                |c, s, a, b, ctx| <Combine<f64, f64, f64, F> as Op>::cycle(c, s, (a, b), ctx),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// The graphs: custom ops chained with built-ins, no `OpKind` rows anywhere.
// ---------------------------------------------------------------------------

wingfoil_next::graph! {
    fn custom_ops_graph(g: &GraphBuilder) -> (Stream<f64>, Stream<u64>) {
        let squares = g.ticker(PERIOD).count().map(|c| (c * c) as f64);
        let smoothed = squares.scale(0.5).delta().apply(|x| x + 1.0);
        let levels = g.ticker(PERIOD).count().map(|c| c / 3).distinct();
        (smoothed, levels)
    }
}

/// All three engines agree, and the values match a hand computation:
/// `0.5·c²` deltas are `c − 0.5` (quiet at c=1), `+1` → `c + 0.5`; at c=8
/// that is `8.5`. `c/3` for c=1..8 is 0,0,0,1,1,1,2,2 → distinct ends at 2.
#[test]
fn custom_ops_compiled_matches_interpreted() {
    let run_for = RunFor::Cycles(8);

    let (mut runner, smoothed, levels) = custom_ops_graph::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let (smoothed_i, levels_i) = (runner.value(smoothed), runner.value(levels));
    assert_eq!(8.5, smoothed_i);
    assert_eq!(2, levels_i);

    let (smoothed_c, levels_c) = custom_ops_graph::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(smoothed_i, smoothed_c);
    assert_eq!(levels_i, levels_c);
}

wingfoil_next::graph! {
    fn scaled_delta(g: &GraphBuilder, src: &Stream<f64>) -> Stream<f64> {
        let out = src.scale(2.0).delta();
        out
    }
}

/// Custom ops inside a compiled **island**: the input-taking graph mounts as
/// one node in an interpreted outer graph; its interior is the same
/// forwarder-monomorphized code `compiled()` emits. `2c²` deltas are
/// `2(2c−1) = 4c−2`; at c=6 that is `22`.
#[test]
fn custom_ops_in_nested_island() {
    let g = GraphBuilder::new();
    let src = g.ticker(PERIOD).count().map(|c| (c * c) as f64);
    let island = scaled_delta::nested(&g, &src);
    let out = island.handle();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(22.0, runner.value(out));
}

wingfoil_next::graph! {
    fn multi_input_graph(g: &GraphBuilder) -> (Stream<f64>, Stream<f64>) {
        let fast = g.ticker(PERIOD).count().map(|c| *c as f64);
        let slow = fast.scale(0.5);
        let spread = fast.spread(&slow);
        let combo = fast.combine(&slow, |a: &f64, b: &f64| a + 10.0 * b);
        (spread, combo)
    }
}

/// **Multi-input custom ops** through the fallback: `&slow` is recognised as
/// a stream edge (it names a bound stream), the closure as config. At cycle
/// c: `fast = c`, `slow = c/2`, so `spread = c/2` and
/// `combo = c + 10·(c/2) = 6c`; at c = 8 that is `4` and `48`.
#[test]
fn multi_input_custom_ops_compiled_matches_interpreted() {
    let run_for = RunFor::Cycles(8);

    let (mut runner, spread, combo) = multi_input_graph::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let (spread_i, combo_i) = (runner.value(spread), runner.value(combo));
    assert_eq!(4.0, spread_i);
    assert_eq!(48.0, combo_i);

    let (spread_c, combo_c) = multi_input_graph::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(spread_i, spread_c);
    assert_eq!(combo_i, combo_c);
}

wingfoil_next::graph! {
    fn spread_island(g: &GraphBuilder, a: &Stream<f64>, b: &Stream<f64>) -> Stream<f64> {
        let out = a.spread(b);
        out
    }
}

/// A multi-input custom op as the body of a **two-input island**: both outer
/// streams feed the composite, the interior is the monomorphized fallback
/// dispatch. `spread = 3c − c = 2c`; at c = 5 that is `10`.
#[test]
fn multi_input_custom_op_in_nested_island() {
    let g = GraphBuilder::new();
    let count = g.ticker(PERIOD).count();
    let fast = count.map(|c| (3 * *c) as f64);
    let slow = count.map(|c| *c as f64);
    let island = spread_island::nested(&g, &fast, &slow);
    let out = island.handle();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(10.0, runner.value(out));
}

// ---------------------------------------------------------------------------
// The two formerly-"exotic" shapes, as USER ops — proving they are ordinary:
// a passive-edge op (sample's shape) and a seeded accumulator (fold's shape).
// The hand-written forwarders below are exactly what `#[op(build = snap,
// passive = [0])]` / `#[op(build = ratchet, init_arg)]` emit for in-crate ops.
// ---------------------------------------------------------------------------

/// User sample: emit the (passive) data value when the trigger ticks.
pub struct Snap<T>(std::marker::PhantomData<T>);

impl<T: Clone + 'static> Op for Snap<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T, &'a ());
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&T, &()),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        Ok(Tick::Value(input.0.clone()))
    }
}

pub const __WF_OP_SNAP_ACTIVATION: Activation = Activation::NONE;
/// Bit 0: the data (receiver) edge is passive; only the trigger activates.
pub const __WF_OP_SNAP_PASSIVE: u32 = 0b1;

pub fn __wf_op_snap_cycle<T: Clone + 'static>(
    cfg: &mut <Snap<T> as Op>::Cfg,
    state: &mut <Snap<T> as Op>::State,
    input: ((&T, bool), (&(), bool)),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<T>> {
    <Snap<T> as Op>::cycle(cfg, state, (input.0.0, input.1.0), ctx)
}

pub fn __wf_op_snap_start<C, S>(_cfg: &mut C, _state: &mut S, _ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}

pub fn __wf_op_snap_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_snap_seed_value<T: Default, P>(_cfg: &P) -> T {
    T::default()
}

pub fn __wf_op_snap_stop<T: Clone + 'static>(
    cfg: &mut <Snap<T> as Op>::Cfg,
    state: &mut <Snap<T> as Op>::State,
    input: ((&T, bool), (&(), bool)),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Snap<T> as Op>::stop(cfg, state, ctx)
}
pub fn __wf_op_snap_teardown<T: Clone + 'static>(
    cfg: &mut <Snap<T> as Op>::Cfg,
    state: &mut <Snap<T> as Op>::State,
    input: ((&T, bool), (&(), bool)),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Snap<T> as Op>::teardown(cfg, state, ctx)
}

/// User seeded accumulator: running max, seeded with (and floored at) `init`.
pub struct Ratchet;

impl Op for Ratchet {
    /// The floor — also the state/value seed.
    type Cfg = f64;
    type State = f64;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut f64,
        state: &mut f64,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        *state = state.max(*input.0);
        Ok(Tick::Value(*state))
    }
}

pub const __WF_OP_RATCHET_ACTIVATION: Activation = Ratchet::ACTIVATION;
pub const __WF_OP_RATCHET_PASSIVE: u32 = 0;

pub fn __wf_op_ratchet_cycle(
    cfg: &mut <Ratchet as Op>::Cfg,
    state: &mut <Ratchet as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<f64>> {
    <Ratchet as Op>::cycle(cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_ratchet_start<C, S>(_cfg: &mut C, _state: &mut S, _ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}

/// Seeded shape: the plain argument (the floor) seeds state *and* slot, so a
/// passive read before the first tick sees the floor, not 0.0.
pub fn __wf_op_ratchet_seed_state(cfg: &f64) -> f64 {
    *cfg
}
pub fn __wf_op_ratchet_seed_value(cfg: &f64) -> f64 {
    *cfg
}

pub fn __wf_op_ratchet_stop(
    cfg: &mut <Ratchet as Op>::Cfg,
    state: &mut <Ratchet as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Ratchet as Op>::stop(cfg, state, ctx)
}
pub fn __wf_op_ratchet_teardown(
    cfg: &mut <Ratchet as Op>::Cfg,
    state: &mut <Ratchet as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = input;
    <Ratchet as Op>::teardown(cfg, state, ctx)
}

trait ShapeOps {
    fn snap(&self, trigger: &Stream<()>) -> Stream<f64>;
    fn ratchet(&self, floor: f64) -> Stream<f64>;
}

impl ShapeOps for Stream<f64> {
    fn snap(&self, trigger: &Stream<()>) -> Stream<f64> {
        let trigger = trigger.handle();
        // bimap: data edge passive, trigger edge active — the public
        // interpreted primitive for the sample shape.
        self.wire(|b, h| b.bimap(h, false, trigger, true, |v: &f64, _: &()| *v))
    }

    fn ratchet(&self, floor: f64) -> Stream<f64> {
        // fold is the public interpreted primitive for the seeded shape —
        // identical math, and it seeds the slot with `floor` just like the
        // compiled emission's seed forwarders.
        self.fold(floor, |acc, v| *acc = acc.max(*v))
    }
}

wingfoil_next::graph! {
    fn shapes_graph(g: &GraphBuilder) -> (Stream<f64>, Stream<f64>) {
        let data = g.ticker(PERIOD).count().map(|c| *c as f64);
        let trig = g.ticker(Duration::from_millis(3));
        let snapped = data.snap(&trig);
        let peak = data.map(|v| 10.0 - (*v - 4.0).abs()).ratchet(5.0);
        (snapped, peak)
    }
}

/// The passive-edge and seeded shapes as user ops. At cycle times 0..6ms:
/// `data = c` (1..7); the 3ms trigger ticks at t ∈ {0, 3, 6}, so `snapped`
/// last updates at t = 6 with `data = 7`. The tent `10 − |c − 4|` runs
/// 7,8,9,10,9,8,7 and `ratchet(5.0)` holds the max: `10`.
#[test]
fn user_passive_and_seeded_shapes() {
    let run_for = RunFor::Cycles(7);

    let (mut runner, snapped, peak) = shapes_graph::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let (snapped_i, peak_i) = (runner.value(snapped), runner.value(peak));
    assert_eq!(7.0, snapped_i);
    assert_eq!(10.0, peak_i);

    let (snapped_c, peak_c) = shapes_graph::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(snapped_i, snapped_c);
    assert_eq!(peak_i, peak_c);
}
