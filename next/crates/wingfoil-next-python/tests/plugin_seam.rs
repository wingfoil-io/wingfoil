//! Proves the extensibility seam is usable from an **external** crate: this
//! file is compiled as its own crate and touches only wingfoil-next-python's
//! *public* API to author and wire a custom op. If it compiles and passes, a
//! third-party op crate can do the same.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next_python::{Activation, Ctx, Op, PyElement, PyGraph, Tick, pyop};

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
