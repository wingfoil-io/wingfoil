//! Proof-of-concept: exposing a `wingfoil-next` **compiled** graph to Python.
//!
//! The rest of `wingfoil-python` binds the classic interpreted engine, where
//! a Python graph is assembled node-by-node at runtime (`ticker(...).map(f)`)
//! and dispatched dynamically. This module instead pins *one* fixed,
//! fully-monomorphized graph defined in Rust with the `wingfoil_next::graph!`
//! macro, and exposes its `compiled()` runner — a straight-line static
//! schedule with every op call inlined — as a Python callable.
//!
//! The `graph!` macro emits three entry points from a single wiring function:
//! `wire()` (the fluent function verbatim), `interpreted()` (built through
//! `wire`, dynamically dispatched), and `compiled(run_mode, run_for)` (the
//! monomorphized straight-line runner). Because both engines are generated
//! from the *same* tokens and call the *same* `Op::cycle` functions, they
//! cannot drift — so this module exposes both to Python, letting the test
//! suite assert they produce identical output and time the difference.
//!
//! Scope, honestly: this is a POC. The graph is fixed and chosen at compile
//! time — `squares` below (ticker -> count -> map(i*i) -> accumulate). This is
//! *not* general Python-defined-graph codegen; the whole point of `compiled()`
//! is that the graph is known and monomorphized ahead of time.

use pyo3::prelude::*;

// The op vocabulary (`ticker`/`count`/`map`/`accumulate`) lives on the
// `SourceOps` / `StreamOps` extension traits, and `GraphBuilder` / `Stream`
// name the wiring types; the `graph!`-generated module pulls them in via
// `use super::*`, so bring the prelude into module scope here.
use wingfoil_next::prelude::*;

use crate::types::{self, ToPyResult};

wingfoil_next::graph! {
    /// A fixed numeric pipeline: a ticker is counted (1, 2, 3, ...), each
    /// count is squared, and the squares are accumulated into a `Vec<u64>`.
    ///
    /// With `cycles = n` the output is `[1, 4, 9, ..., n*n]`. This is the
    /// work `compiled()` monomorphizes into straight-line code, versus the
    /// interpreted engine's per-op dynamic dispatch — both generated from
    /// this one wiring definition, so they cannot drift.
    fn squares(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let period = std::time::Duration::from_millis(10);
        let acc = g.ticker(period).count().map(|i| *i * *i).accumulate();
        acc
    }
}

/// Run the fixed `squares` graph through the **compiled** (monomorphized,
/// static-schedule) runner and return the accumulated squares.
///
/// Signature mirrors `Node.run` / `Graph.run` in the classic bindings so the
/// same `realtime` / `start` / `duration` / `cycles` arguments select the
/// `RunMode` / `RunFor`.
#[pyfunction]
#[pyo3(signature = (realtime, start=None, duration=None, cycles=None))]
pub fn compiled_squares(
    py: Python<'_>,
    realtime: bool,
    start: Option<Py<PyAny>>,
    duration: Option<Py<PyAny>>,
    cycles: Option<u32>,
) -> PyResult<Vec<u64>> {
    let (run_mode, run_for) =
        types::parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;

    // The compiled runner owns its own kernel and run loop; release the GIL
    // for the duration of the (potentially real-time) run.
    let result = py.detach(move || squares::compiled(run_mode, run_for));
    let (values,) = result.to_pyresult()?;
    Ok(values)
}

/// Run the *same* fixed `squares` graph through the classic-style
/// **interpreted** engine (dynamic dispatch), returning the accumulated
/// squares. Provided so Python can assert the two engines agree and compare
/// their timing.
#[pyfunction]
#[pyo3(signature = (realtime, start=None, duration=None, cycles=None))]
pub fn interpreted_squares(
    py: Python<'_>,
    realtime: bool,
    start: Option<Py<PyAny>>,
    duration: Option<Py<PyAny>>,
    cycles: Option<u32>,
) -> PyResult<Vec<u64>> {
    let (run_mode, run_for) =
        types::parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;

    let values = py.detach(move || -> anyhow::Result<Vec<u64>> {
        let (mut runner, acc) = squares::interpreted();
        runner.run(run_mode, run_for)?;
        Ok(runner.value(acc))
    });
    values.to_pyresult()
}
