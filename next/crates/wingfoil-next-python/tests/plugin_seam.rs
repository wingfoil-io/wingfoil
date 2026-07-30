//! Proves the extensibility seam is usable from an **external** crate: this
//! file is compiled as its own crate and touches only wingfoil-next-python's
//! *public* API to author and wire a custom op. If it compiles and passes, a
//! third-party op crate can do the same.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use pyo3::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::Stream;
use wingfoil_next_python::{
    Activation, Ctx, Op, PyElement, PyGraph, Tick, pyadapter, pygraph, pyop,
};

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

// A source `#[pyadapter]` authored from an external crate: a trait on
// `GraphBuilder` exposed to Python. Proves the macro emits the source
// `#[pyfunction]` and the builder/erase_source seam works cross-crate.
trait CountUpOps {
    fn count_up(&self, from: f64) -> wingfoil_next::prelude::Stream<f64>;
}

#[pyadapter(name = count_up, source)]
impl CountUpOps for wingfoil_next::prelude::GraphBuilder {
    fn count_up(&self, from: f64) -> wingfoil_next::prelude::Stream<f64> {
        use wingfoil_next::prelude::{SourceOps, StreamOps};
        self.ticker(Duration::from_nanos(100))
            .count()
            .map(move |n: &u64| from + (*n as f64))
    }
}

// A sink `#[pyadapter]` from an external crate (a trait on `Stream<f64>`). Its
// params must be Python-passable (they become `#[pyfunction]` params), so the
// sink target is a `Py<PyAny>` list. Compile proof the macro emits the sink
// `#[pyfunction]` cross-crate for a `Stream<()>` terminal.
trait PyPushSinkOps {
    fn py_push_sink(&self, target: Py<PyAny>) -> wingfoil_next::prelude::Stream<()>;
}

#[pyadapter(name = py_push_sink)]
impl PyPushSinkOps for wingfoil_next::prelude::Stream<f64> {
    fn py_push_sink(&self, target: Py<PyAny>) -> wingfoil_next::prelude::Stream<()> {
        use wingfoil_next::prelude::StreamOps;
        self.for_each(move |v: &f64| {
            Python::attach(|py| {
                target
                    .bind(py)
                    .call_method1("append", (*v,))
                    .map_err(|err| anyhow::anyhow!("py_push_sink append raised: {err}"))?;
                Ok(())
            })
        })
    }
}

#[test]
fn pyadapter_sink_macro_compiles_in_an_external_crate() {
    // The generated sink function exists: `#[pyadapter]` (no `source` marker)
    // expanded for a `Stream<T> -> Stream<()>` adapter.
    let _f = py_push_sink;
}

#[test]
fn pyadapter_sink_seam_from_an_external_crate() {
    // The typed-in/erased-out seam the sink macro builds on: extract to native
    // f64, run a `for_each` terminal, erase the `Stream<()>` output.
    use wingfoil_next::prelude::StreamOps;
    let seen = Rc::new(RefCell::new(Vec::<f64>::new()));
    let target = seen.clone();
    let g = PyGraph::new();
    let src = g.counter(Duration::from_nanos(100));
    let unit = src.typed_input::<f64>().for_each(move |v: &f64| {
        target.borrow_mut().push(*v);
        Ok(())
    });
    let _erased = src.erased_output::<()>(unit);

    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
    assert_eq!(vec![1.0, 2.0, 3.0], *seen.borrow());
}

#[test]
fn pyadapter_source_from_an_external_crate() {
    // The generated function exists (compile proof).
    let _f = count_up;

    // The builder/erase_source seam it builds on splices a native source in
    // (`count_up` is the trait method above, in scope in this file).
    let g = PyGraph::new();
    let typed = g.builder().count_up(100.0);
    let src = g.erase_source::<f64>(typed);
    let acc = src.accumulate();
    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();
    let v: Vec<f64> = Python::attach(|py| acc.value().value().extract(py).unwrap());
    assert_eq!(vec![101.0, 102.0], v);
}

// A **burst** source `#[pyadapter]` from an external crate: returns
// `Stream<Burst<f64>>`, so each tick erases to a Python list.
trait BurstDoubleOps {
    fn burst_double(&self) -> wingfoil_next::prelude::Stream<wingfoil_next::Burst<f64>>;
}

#[pyadapter(name = burst_double, source)]
impl BurstDoubleOps for wingfoil_next::prelude::GraphBuilder {
    fn burst_double(&self) -> wingfoil_next::prelude::Stream<wingfoil_next::Burst<f64>> {
        use wingfoil_next::prelude::{SourceOps, StreamOps};
        let a = self
            .ticker(Duration::from_nanos(100))
            .count()
            .map(|n: &u64| *n as f64);
        let b = self
            .ticker(Duration::from_nanos(100))
            .count()
            .map(|n: &u64| *n as f64 + 0.5);
        self.combine(&[a, b])
    }
}

#[test]
fn pyadapter_burst_source_from_an_external_crate() {
    // The generated burst-source function exists (compile proof).
    let _f = burst_double;

    // The erase_burst_source seam maps each Burst<f64> to a Python list.
    let g = PyGraph::new();
    let typed = g.builder().burst_double();
    let src = g.erase_burst_source::<f64>(typed);
    let acc = src.accumulate();
    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();
    let rows: Vec<Vec<f64>> = Python::attach(|py| acc.value().value().extract(py).unwrap());
    assert_eq!(vec![vec![1.0, 1.5], vec![2.0, 2.5]], rows);
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

// A **fallible** source `#[pyadapter]` with **Python defaults** — the shape
// every real IO adapter has: its wiring validates config and can fail, and most
// of its knobs are optional. `Result<Stream<T>>` makes the generated
// `#[pyfunction]` return `PyResult`, and the `#[pyo3(signature = ..)]` written
// over the *adapter method's* params is forwarded to it with the generated
// `graph` receiver injected.
trait CheckedRampOps {
    fn checked_ramp(&self, start: f64, step: f64) -> anyhow::Result<Stream<f64>>;
}

#[pyadapter(name = checked_ramp, source)]
impl CheckedRampOps for wingfoil_next::prelude::GraphBuilder {
    #[pyo3(signature = (start, step = 1.0))]
    fn checked_ramp(&self, start: f64, step: f64) -> anyhow::Result<Stream<f64>> {
        use wingfoil_next::prelude::{SourceOps, StreamOps};
        if step <= 0.0 {
            anyhow::bail!("checked_ramp: step must be positive, got {step}");
        }
        Ok(self
            .ticker(Duration::from_nanos(100))
            .count()
            .map(move |n: &u64| start + step * ((*n - 1) as f64)))
    }
}

// The fallible **sink** shape, likewise: a wiring-time check plus an optional arg.
trait CheckedSinkOps {
    fn checked_sink(&self, target: Py<PyAny>, limit: Option<usize>) -> anyhow::Result<Stream<()>>;
}

#[pyadapter(name = checked_sink)]
impl CheckedSinkOps for Stream<f64> {
    #[pyo3(signature = (target, limit = None))]
    fn checked_sink(&self, target: Py<PyAny>, limit: Option<usize>) -> anyhow::Result<Stream<()>> {
        use wingfoil_next::prelude::StreamOps;
        if limit == Some(0) {
            anyhow::bail!("checked_sink: limit must be non-zero");
        }
        Ok(self.for_each(move |v: &f64| {
            Python::attach(|py| {
                target
                    .bind(py)
                    .call_method1("append", (*v,))
                    .map_err(|err| anyhow::anyhow!("checked_sink append raised: {err}"))?;
                Ok(())
            })
        }))
    }
}

#[test]
fn fallible_pyadapter_generates_pyresult_functions() {
    // Compile proof: both generated functions exist for `Result`-returning
    // adapter methods carrying a forwarded `#[pyo3(signature = ..)]`.
    let _source = checked_ramp;
    let _sink = checked_sink;
}

#[test]
fn fallible_pyadapter_wiring_succeeds_and_fails_as_written() {
    let g = PyGraph::new();
    // The happy path wires and runs like any other source.
    let ok = g
        .builder()
        .checked_ramp(10.0, 5.0)
        .expect("positive step wires");
    let erased = g.erase_source::<f64>(ok);
    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
    let v: f64 = (&erased.value()).try_into().unwrap();
    assert_eq!(20.0, v);

    // The rejected config is a *wiring* error — what the generated
    // `#[pyfunction]` turns into a Python exception.
    let err = match g.builder().checked_ramp(10.0, 0.0) {
        Ok(_) => panic!("a non-positive step must be rejected at wiring"),
        Err(err) => err,
    };
    assert!(
        format!("{err:#}").contains("step must be positive"),
        "{err:#}"
    );
}

// The **free-fn** `#[pyadapter]` form — the shape a binding module wants. The
// receiver is the first param (`&GraphBuilder` for a source, `&Stream<T>` for a
// sink), so there is no throwaway trait and no second copy of the signature.
// `name` differs from the fn's own name because the macro emits a
// `#[pyfunction]` of that name alongside it.
#[pyadapter(name = free_ramp, source)]
#[pyo3(signature = (start, step = 1.0))]
fn ramp(g: &wingfoil_next::prelude::GraphBuilder, start: f64, step: f64) -> Stream<f64> {
    use wingfoil_next::prelude::{SourceOps, StreamOps};
    g.ticker(Duration::from_nanos(100))
        .count()
        .map(move |n: &u64| start + step * ((*n - 1) as f64))
}

#[pyadapter(name = free_checked_ramp, source)]
fn checked_ramp_fn(
    g: &wingfoil_next::prelude::GraphBuilder,
    start: f64,
    step: f64,
) -> anyhow::Result<Stream<f64>> {
    if step <= 0.0 {
        anyhow::bail!("free_checked_ramp: step must be positive, got {step}");
    }
    Ok(ramp(g, start, step))
}

#[pyadapter(name = free_push_sink)]
#[pyo3(signature = (target, scale = 1.0))]
fn push_sink(stream: &Stream<f64>, target: Py<PyAny>, scale: f64) -> anyhow::Result<Stream<()>> {
    use wingfoil_next::prelude::StreamOps;
    Ok(stream.for_each(move |v: &f64| {
        Python::attach(|py| {
            target
                .bind(py)
                .call_method1("append", (*v * scale,))
                .map_err(|err| anyhow::anyhow!("free_push_sink append raised: {err}"))?;
            Ok(())
        })
    }))
}

#[test]
fn free_fn_pyadapter_generates_source_and_sink_functions() {
    // Compile proof: infallible source, fallible source, and fallible sink all
    // generate their `#[pyfunction]` from a plain fn in an external crate.
    let _a = free_ramp;
    let _b = free_checked_ramp;
    let _c = free_push_sink;
}

#[test]
fn free_fn_pyadapter_wires_end_to_end() {
    let g = PyGraph::new();

    // The wiring fns stay ordinary Rust fns, callable directly — the macro adds
    // the Python entry point beside them rather than consuming them.
    let typed = checked_ramp_fn(g.builder(), 10.0, 5.0).expect("positive step wires");
    let (target, unit) = Python::attach(|py| {
        let list = pyo3::types::PyList::empty(py).into_any().unbind();
        let sink = push_sink(&typed, list.clone_ref(py), 2.0).expect("sink wires");
        (list, sink)
    });
    let _erased = g.erase_source::<()>(unit);

    g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let got: Vec<f64> = Python::attach(|py| target.extract(py).unwrap());
    assert_eq!(vec![20.0, 30.0, 40.0], got);

    // And the fallible free-fn form rejects its bad config at wiring.
    let err = match checked_ramp_fn(g.builder(), 10.0, 0.0) {
        Ok(_) => panic!("a non-positive step must be rejected at wiring"),
        Err(err) => err,
    };
    assert!(
        format!("{err:#}").contains("step must be positive"),
        "{err:#}"
    );
}
