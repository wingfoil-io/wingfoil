//! **The out-of-crate `#[op]` proof** (#782), as an independently compiled
//! crate.
//!
//! trybuild builds this file as its own binary crate in a throwaway Cargo
//! project, with `wingfoil` as an ordinary path dependency — no workspace, no
//! `extern crate self as wingfoil`, nothing that could make `crate::…` resolve
//! to the engine. So it fails outright against a `#[op]` that emits
//! `impl crate::interp::Builder`, and passing it means the attribute really is
//! usable by a downstream author.
//!
//! What it pins is the *whole* authoring story the issue asks for — an
//! `impl Op`, the attribute, and a three-line fluent method:
//!
//! - `Gain` — plain `Cfg`, hand-written fluent method over the generated
//!   extension trait;
//! - `Runs` — state (`Cfg = ()`), fluent method **generated** by
//!   `#[op(fluent)]`, proving that macro's `::wingfoil::`-qualified body
//!   resolves out-of-crate too;
//!
//! and runs them through all three engines — `interpreted()`, `compiled()`
//! and a `nested()` island — asserting the three agree.

use std::time::Duration;

use wingfoil::anyhow::Result;
use wingfoil::op;
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

/// Multiply by a constant. The smallest possible user op.
pub struct Gain;

#[op(build = gain)]
impl Op for Gain {
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

/// Length of the current run of non-decreasing values — a stateful op, so the
/// state local the emission declares without a type has something to infer.
pub struct Runs;

#[op(build = runs, fluent)]
impl Op for Runs {
    type Cfg = ();
    type State = Option<(f64, u64)>;
    type In<'a> = (&'a f64,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<(f64, u64)>,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        let v = *input.0;
        let n = match *state {
            Some((prev, n)) if v >= prev => n + 1,
            _ => 1,
        };
        *state = Some((v, n));
        Ok(Tick::Value(n))
    }
}

/// The three-line fluent method — all that is left to write by hand. It calls
/// `Builder::gain`, which arrives on the generated `__WfBuildGain` extension
/// trait (in scope here because `Gain` is declared in this file).
trait UserOps {
    fn gain(&self, factor: f64) -> Stream<f64>;
    fn runs(&self) -> Stream<u64>;
}

impl UserOps for Stream<f64> {
    fn gain(&self, factor: f64) -> Stream<f64> {
        self.wire(move |b, h| b.gain(h, factor))
    }

    // …and this one is not written at all: `#[op(fluent)]` wrote it.
    __wf_fluent_runs!();
}

wingfoil::nitro! {
    fn user_graph(g: &GraphBuilder) -> (Stream<f64>, Stream<u64>) {
        let c = g.ticker(PERIOD).count().map(|c| *c as f64);
        let scaled = c.gain(2.0);
        let streak = scaled.runs();
        (scaled, streak)
    }
}

wingfoil::nitro! {
    fn user_island(g: &GraphBuilder, src: &Stream<f64>) -> Stream<f64> {
        let out = src.gain(3.0);
        out
    }
}

fn main() {
    let run_for = RunFor::Cycles(5);

    let (mut runner, scaled, streak) = user_graph::interpreted();
    runner.run(HISTORICAL, run_for).expect("interpreted run");
    let (scaled_i, streak_i) = (runner.value(scaled), runner.value(streak));
    assert_eq!(10.0, scaled_i);
    assert_eq!(5, streak_i);

    let (scaled_c, streak_c) = user_graph::compiled(HISTORICAL, run_for).expect("compiled run");
    assert_eq!(scaled_i, scaled_c);
    assert_eq!(streak_i, streak_c);

    // …and the same op inside a `nested()` island.
    let g = GraphBuilder::new();
    let src = g.ticker(PERIOD).count().map(|c| *c as f64);
    let island = user_island::nested(&g, &src);
    let out = island.handle();
    let mut runner = g.build();
    runner.run(HISTORICAL, run_for).expect("island run");
    assert_eq!(15.0, runner.value(out));
}
