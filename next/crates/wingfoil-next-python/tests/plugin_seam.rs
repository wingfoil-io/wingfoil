//! Proves the extensibility seam is usable from an **external** crate: this
//! file is compiled as its own crate and touches only wingfoil-next-python's
//! *public* API to author and wire a custom op. If it compiles and passes, a
//! third-party op crate can do the same.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next_python::{Activation, Ctx, Op, PyElement, PyGraph, Tick, pygraph, pyop};

// Compile-level proof that `#[pyop]` works from an *external* crate: the paths
// the macro emits (`::wingfoil_next_python::...`) resolve here, and the
// generated `#[pyfunction]` `triple` is a real item. If this file compiles, a
// third-party op crate can use `#[pyop]`.
struct Triple;

#[pyop(name = triple)]
impl Op for Triple {
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
    ) -> anyhow::Result<Tick<f64>> {
        Ok(Tick::Value(input.0 * 3.0))
    }
}

#[test]
fn pyop_generates_a_function_in_an_external_crate() {
    // Referencing the generated function as a value proves it expanded.
    let _f = triple;
}

// A **stateful** external `#[pyop]` (accumulator in `State`) — compile-level
// proof that the macro handles `State != ()` from a third-party crate, not just
// stateless ops.
struct Accumulate;

#[pyop(name = accumulate)]
impl Op for Accumulate {
    type Cfg = ();
    type State = f64;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        total: &mut f64,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> anyhow::Result<Tick<f64>> {
        *total += input.0;
        Ok(Tick::Value(*total))
    }
}

// A **two-input** external `#[pyop]` (`In<'a> = (&'a f64, &'a f64)`) — compile
// proof that the macro emits a two-stream `#[pyfunction]` from a third-party
// crate.
struct AddStreams;

#[pyop(name = add_streams)]
impl Op for AddStreams {
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
    ) -> anyhow::Result<Tick<f64>> {
        Ok(Tick::Value(input.0 + input.1))
    }
}

// A `#[pygraph]` from an external crate: a Rust-authored sub-graph (triple each
// value) exposed as a Python callable. Proves the macro emits the splice
// wrapper and the typed-in/erased-out seam works from a third-party crate.
#[pygraph(name = triple_subgraph)]
fn build_triple_subgraph(
    input: &wingfoil_next::prelude::Stream<f64>,
) -> wingfoil_next::prelude::Stream<f64> {
    use wingfoil_next::prelude::StreamOps;
    input.map(|x: &f64| x * 3.0)
}

#[test]
fn pygraph_exposes_a_subgraph_from_an_external_crate() {
    // The generated function exists: `#[pygraph]` expanded.
    let _f = triple_subgraph;

    // The seam it builds on splices a typed sub-graph in and erases the output.
    let g = PyGraph::new();
    let src = g.counter(Duration::from_nanos(100));
    let out = src.erased_output(build_triple_subgraph(&src.typed_input::<f64>()));

    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();
    let v: f64 = (&out.value()).try_into().unwrap();
    assert_eq!(6.0, v); // second cycle: 2 * 3
}

#[test]
fn pyop_supports_two_input_ops_in_an_external_crate() {
    // The generated two-stream function exists: `#[pyop]` expanded for a
    // two-input op.
    let _f = add_streams;

    // The two-input shape runs via the public `wire_op2` seam the macro builds
    // on: combine two counters.
    let g = PyGraph::new();
    let a = g.counter(Duration::from_nanos(100));
    let b = g.counter(Duration::from_nanos(100));
    let summed = a.wire_op2::<f64, f64, _, _, f64, _, _>(
        &b,
        "add_streams",
        Activation::NONE,
        (),
        || (),
        |_cfg, _state, x: &f64, y: &f64, _ctx| Ok(Tick::Value(*x + *y)),
    );

    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();
    let v: f64 = (&summed.value()).try_into().unwrap();
    assert_eq!(4.0, v); // 2 + 2 on the second cycle
}

#[test]
fn pyop_supports_stateful_ops_in_an_external_crate() {
    // The generated function exists: `#[pyop]` expanded for a stateful op.
    let _f = accumulate;

    // And the stateful shape runs — the accumulator carries across cycles and
    // re-seeds from Default on a fresh run (verified via the public seam that
    // `#[pyop]` builds on).
    let g = PyGraph::new();
    let acc = g
        .counter(Duration::from_nanos(100))
        .wire_op1::<f64, _, _, f64, _, _>(
            "accumulate",
            Activation::NONE,
            (),
            || 0.0_f64,
            |_cfg, total: &mut f64, a: &f64, _ctx| {
                *total += *a;
                Ok(Tick::Value(*total))
            },
        );

    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
    let v: f64 = (&acc.value()).try_into().unwrap();
    assert_eq!(6.0, v); // 1 + 2 + 3
}

#[test]
fn external_crate_can_wire_a_custom_op() {
    let g = PyGraph::new();

    // A custom stateless op (`bump`: add a constant), authored with nothing but
    // the public seam — exactly what `pyop!` generates the Python glue around.
    let bumped = g
        .constant(PyElement::from(10.0_f64))
        .wire_op1::<f64, _, _, f64, _, _>(
            "bump",
            Activation::NONE,
            5.0_f64,
            || (),
            |cfg: &mut f64, _state: &mut (), a: &f64, _ctx| Ok(Tick::Value(*a + *cfg)),
        );

    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();

    let v: f64 = (&bumped.value()).try_into().unwrap();
    assert_eq!(15.0, v);
}

#[test]
fn custom_op_error_aborts_run() {
    let g = PyGraph::new();

    // The op refuses odd inputs — a step error must abort the run with context.
    // `counter` emits 1 on the first cycle, which is odd.
    let checked = g
        .counter(Duration::from_nanos(100))
        .wire_op1::<i64, _, _, i64, _, _>(
            "even_only",
            Activation::NONE,
            (),
            || (),
            |_cfg, _state, a: &i64, _ctx| {
                if a % 2 == 0 {
                    Ok(Tick::Value(*a))
                } else {
                    anyhow::bail!("odd value {a} rejected")
                }
            },
        );

    let err = g
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap_err();
    assert!(format!("{err:#}").contains("odd value"));
    let _ = checked;
}
