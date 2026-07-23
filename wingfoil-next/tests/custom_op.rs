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
//! Three op shapes are exercised, all defined *in this test crate* (i.e. from
//! the library's point of view: user code):
//!
//! - [`Scale`] — plain config (`Cfg = f64`), no state;
//! - [`Delta<T>`] — **generic** op with inferred state (`State = Option<T>`);
//! - [`Apply<F>`] — **closure config** (`Cfg = F`), via the `_cycle_owned`
//!   forwarder (the same inference-deferral trick as `cycle_owned_cfg`);
//!
//! plus in-crate [`Distinct`](wingfoil_next::ops) — which has `#[op]` but **no
//! table row**, proving the whole `#[op]` catalog reaches `graph!` through the
//! fallback with zero per-op macro edits.

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

// ---------------------------------------------------------------------------
// The forwarders `graph!`'s generic fallback calls by naming convention —
// hand-written here as what a user-facing `#[op]` derive would generate.
// (The in-crate `#[op]` attribute already generates exactly these for the
// catalog ops; it isn't usable out-of-crate yet because it also emits an
// inherent `impl Builder`.)
// ---------------------------------------------------------------------------

pub const __WF_OP_SCALE_ACTIVATION: Activation = Scale::ACTIVATION;
pub const __WF_OP_DELTA_ACTIVATION: Activation = Activation::NONE;
pub const __WF_OP_APPLY_ACTIVATION: Activation = Activation::NONE;

pub fn __wf_op_scale_cycle(
    cfg: &mut <Scale as Op>::Cfg,
    state: &mut <Scale as Op>::State,
    input: <Scale as Op>::In<'_>,
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Scale as Op>::Out>> {
    <Scale as Op>::cycle(cfg, state, input, ctx)
}

pub fn __wf_op_scale_start(
    cfg: &mut <Scale as Op>::Cfg,
    state: &mut <Scale as Op>::State,
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    <Scale as Op>::start(cfg, state, ctx)
}

pub fn __wf_op_delta_cycle<T: Clone + std::ops::Sub<Output = T> + 'static>(
    cfg: &mut <Delta<T> as Op>::Cfg,
    state: &mut <Delta<T> as Op>::State,
    input: <Delta<T> as Op>::In<'_>,
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Delta<T> as Op>::Out>> {
    <Delta<T> as Op>::cycle(cfg, state, input, ctx)
}

pub fn __wf_op_delta_start<T: Clone + std::ops::Sub<Output = T> + 'static>(
    cfg: &mut <Delta<T> as Op>::Cfg,
    state: &mut <Delta<T> as Op>::State,
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    <Delta<T> as Op>::start(cfg, state, ctx)
}

pub fn __wf_op_apply_cycle_owned<F: Fn(&f64) -> f64 + 'static>(
    mut cfg: <Apply<F> as Op>::Cfg,
    state: &mut <Apply<F> as Op>::State,
    input: <Apply<F> as Op>::In<'_>,
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Apply<F> as Op>::Out>> {
    <Apply<F> as Op>::cycle(&mut cfg, state, input, ctx)
}

pub fn __wf_op_apply_start_owned<F: Fn(&f64) -> f64 + 'static>(
    mut cfg: <Apply<F> as Op>::Cfg,
    state: &mut <Apply<F> as Op>::State,
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    <Apply<F> as Op>::start(&mut cfg, state, ctx)
}

// ---------------------------------------------------------------------------
// The fluent methods, so `wire()`/`interpreted()` see the same vocabulary —
// one-liners over the (now public) single-input registration primitive.
// ---------------------------------------------------------------------------

trait CustomOps {
    fn scale(&self, factor: f64) -> Stream<f64>;
    fn delta(&self) -> Stream<f64>;
    fn apply<F: Fn(&f64) -> f64 + 'static>(&self, f: F) -> Stream<f64>;
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
