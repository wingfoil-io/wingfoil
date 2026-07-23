//! Table row vs generic fallback: is the forwarder-emitted (inference-driven)
//! `compiled()` path as fast as a hand-written `OpKind` table row?
//!
//! Two graphs with identical shape and semantics — a `count` source into a
//! 20-deep dense chain of `x + 1.0` stages, every node firing every cycle:
//!
//! - `table_chain` — the stages are `.map_n(20, |v| *v + 1.0)`: the built-in
//!   `Map` table row, closure config through `cycle_owned_cfg`;
//! - `custom_chain` — the stages are 20 `.incr()` calls: a user op with **no
//!   table row**, emitted through the generic fallback's naming-convention
//!   forwarders (including the conservative `__dirty` check the fallback
//!   adds because it cannot see `ACTIVATION`).
//!
//! Both are fully monomorphized by the same `graph!` expansion machinery, so
//! any gap is the cost of the generic path itself.

use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::anyhow::Result;
use wingfoil_next::op::{Activation, Ctx, Op, Tick};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

/// The custom stage: `x + 1.0`, semantically identical to the map closure.
pub struct Incr;

impl Op for Incr {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(input.0 + 1.0))
    }
}

pub const __WF_OP_INCR_ACTIVATION: Activation = Incr::ACTIVATION;
pub const __WF_OP_INCR_PASSIVE: u32 = 0;

pub fn __wf_op_incr_cycle(
    cfg: &mut <Incr as Op>::Cfg,
    state: &mut <Incr as Op>::State,
    input: ((&f64, bool),),
    ctx: &mut Ctx<'_>,
) -> Result<Tick<<Incr as Op>::Out>> {
    <Incr as Op>::cycle(cfg, state, (input.0.0,), ctx)
}

pub fn __wf_op_incr_start<C, S>(_cfg: &mut C, _state: &mut S, _ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}

pub fn __wf_op_incr_seed_state<P>(_cfg: &P) {}
pub fn __wf_op_incr_seed_value<P>(_cfg: &P) -> f64 {
    0.0
}

trait IncrOps {
    fn incr(&self) -> Stream<f64>;
}

impl IncrOps for Stream<f64> {
    fn incr(&self) -> Stream<f64> {
        self.wire(|b, h| {
            b.register_op1(h, "incr", Incr::ACTIVATION, (), (), |c, s, a, ctx| {
                <Incr as Op>::cycle(c, s, (a,), ctx)
            })
        })
    }
}

wingfoil_next::graph! {
    fn table_chain(g: &GraphBuilder) -> Stream<f64> {
        let out = g
            .ticker(PERIOD)
            .count()
            .map(|c| *c as f64)
            .map_n(20, |v| *v + 1.0);
        out
    }
}

wingfoil_next::graph! {
    fn custom_chain(g: &GraphBuilder) -> Stream<f64> {
        let out = g
            .ticker(PERIOD)
            .count()
            .map(|c| *c as f64)
            .incr().incr().incr().incr().incr()
            .incr().incr().incr().incr().incr()
            .incr().incr().incr().incr().incr()
            .incr().incr().incr().incr().incr();
        out
    }
}

fn dense_chain(c: &mut Criterion) {
    // ticker + count + cast-map + 20 stages.
    const NODES: u64 = 23;
    const CYCLES: u32 = 10_000;
    let run_for = RunFor::Cycles(CYCLES);

    // All four paths must agree before being compared for speed.
    let (table,) = table_chain::compiled(HISTORICAL, run_for).unwrap();
    let (custom,) = custom_chain::compiled(HISTORICAL, run_for).unwrap();
    let (mut runner, out) = custom_chain::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    assert_eq!(table, custom);
    assert_eq!(table, runner.value(out));
    assert_eq!(table, f64::from(CYCLES) + 20.0);

    let mut g = c.benchmark_group("dense_chain_20");
    g.sample_size(30);
    g.throughput(Throughput::Elements(CYCLES as u64 * NODES));

    g.bench_function("table_map_n_compiled", |b| {
        b.iter(|| black_box(table_chain::compiled(HISTORICAL, run_for).unwrap()))
    });

    g.bench_function("custom_fallback_compiled", |b| {
        b.iter(|| black_box(custom_chain::compiled(HISTORICAL, run_for).unwrap()))
    });

    g.bench_function("custom_interpreted", |b| {
        b.iter(|| {
            let (mut runner, out) = custom_chain::interpreted();
            runner.run(HISTORICAL, run_for).unwrap();
            black_box(runner.value(out))
        })
    });

    g.finish();
}

criterion_group!(benches, dense_chain);
criterion_main!(benches);
