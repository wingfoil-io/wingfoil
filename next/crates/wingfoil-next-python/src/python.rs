//! `#[pyclass]` glue: the Python-facing `Graph` and `Stream` classes and the
//! `wingfoil_next` module.
//!
//! This is the thin exposure layer over the erased object form in
//! [`crate::graph`]. It does two jobs and nothing else:
//!
//! 1. **Edge conversion** — native Python values cross as `Py<PyAny>` and are
//!    boxed into / unboxed from [`PyElement`] at the seam; run arguments and
//!    durations arrive as Python-friendly scalars (nanosecond ints, flags) and
//!    become [`RunMode`]/[`RunFor`]/[`Duration`].
//! 2. **Error mapping** — an `anyhow::Error` from a run becomes a Python
//!    exception.
//!
//! The classes are `unsendable`: the graph is `Rc`-based (single-threaded
//! construction), so pyo3 pins each object to the thread that created it.

use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::graph::{PyGraph, PyStream};
use crate::{Activation, Ctx, Op, PyElement, Tick, pyop};

fn to_pyerr(err: anyhow::Error) -> PyErr {
    PyRuntimeError::new_err(format!("{err:#}"))
}

/// A held wingfoil-next graph. Build sources on it, wire them with
/// [`Stream`] combinators, then [`run`](Graph::run).
#[pyclass(name = "Graph", unsendable)]
pub struct Graph(PyGraph);

#[pymethods]
impl Graph {
    #[new]
    fn new() -> Self {
        Graph(PyGraph::new())
    }

    /// A source that ticks once with `value` on the first cycle.
    fn constant(&self, value: Py<PyAny>) -> Stream {
        Stream(self.0.constant(PyElement::from(value)))
    }

    /// A source that emits the running tick count `1, 2, 3, …` every
    /// `period_nanos` nanoseconds.
    fn counter(&self, period_nanos: u64) -> Stream {
        Stream(self.0.counter(Duration::from_nanos(period_nanos)))
    }

    /// Run the graph to its bound.
    ///
    /// `realtime` selects wall-clock vs historical replay (from `start_nanos`).
    /// The bound is `cycles` if given, else `duration_nanos`, else runs forever.
    /// A producer/callable error aborts the run and is raised as an exception.
    #[pyo3(signature = (cycles=None, duration_nanos=None, realtime=false, start_nanos=0))]
    fn run(
        &self,
        cycles: Option<u32>,
        duration_nanos: Option<u64>,
        realtime: bool,
        start_nanos: u64,
    ) -> PyResult<()> {
        let run_mode = if realtime {
            RunMode::RealTime
        } else {
            RunMode::HistoricalFrom(NanoTime::from(start_nanos))
        };
        let run_for = match (cycles, duration_nanos) {
            (Some(c), _) => RunFor::Cycles(c),
            (None, Some(d)) => RunFor::Duration(Duration::from_nanos(d)),
            (None, None) => RunFor::Forever,
        };
        self.0.run(run_mode, run_for).map_err(to_pyerr)
    }

    /// Wire a Python object as a graph node. The object is activated by its
    /// `upstreams`' ticks and each cycle must implement the protocol
    /// `cycle(values) -> bool` (given the upstreams' current values, return
    /// whether it ticked) and `peek()` (its output value when it ticked). A
    /// raised exception aborts the run. See [`PyGraph::custom_node`].
    fn custom_node(&self, upstreams: Vec<PyRef<'_, Stream>>, obj: Py<PyAny>) -> Stream {
        let ups: Vec<PyStream> = upstreams.iter().map(|s| s.0.clone()).collect();
        Stream(self.0.custom_node(ups, obj))
    }
}

/// A stream in a [`Graph`]. Combinators return new streams on the same graph;
/// [`value`](Stream::value) reads the current value back after a run.
#[pyclass(name = "Stream", unsendable)]
pub struct Stream(PyStream);

impl Stream {
    /// The underlying erased object form — the seam third-party ops wire onto
    /// via [`PyStream::wire_op1`]. Lets a `#[pyfunction]` in any crate accept a
    /// `Stream` and extend it.
    pub fn object(&self) -> &PyStream {
        &self.0
    }
}

impl From<PyStream> for Stream {
    fn from(stream: PyStream) -> Self {
        Stream(stream)
    }
}

#[pymethods]
impl Stream {
    /// Apply a Python callable to each value; a raised exception aborts the run.
    fn map(&self, func: Py<PyAny>) -> Stream {
        Stream(self.0.map(func))
    }

    /// Emit only when `condition`'s current value is truthy.
    fn filter(&self, condition: PyRef<'_, Stream>) -> Stream {
        Stream(self.0.filter(&condition.0))
    }

    /// Merge with another stream; the earliest-supplied ticked input wins.
    fn merge(&self, other: PyRef<'_, Stream>) -> Stream {
        Stream(self.0.merge(&other.0))
    }

    /// Re-emit each value `delay_nanos` nanoseconds later.
    fn delay(&self, delay_nanos: u64) -> Stream {
        Stream(self.0.delay(Duration::from_nanos(delay_nanos)))
    }

    /// Suppress consecutive duplicate values (emit on change only).
    fn distinct(&self) -> Stream {
        Stream(self.0.distinct())
    }

    /// Emit the running tick count `1, 2, 3, …`, ignoring the values.
    fn count(&self) -> Stream {
        Stream(self.0.count())
    }

    /// Pass through the first `limit` values, then stay quiet.
    fn limit(&self, limit: u32) -> Stream {
        Stream(self.0.limit(limit))
    }

    /// Rate-limit: emit at most once per `interval_nanos` nanoseconds.
    fn throttle(&self, interval_nanos: u64) -> Stream {
        Stream(self.0.throttle(Duration::from_nanos(interval_nanos)))
    }

    /// Emit this stream's current value whenever `trigger` ticks.
    fn sample(&self, trigger: PyRef<'_, Stream>) -> Stream {
        Stream(self.0.sample(&trigger.0))
    }

    /// Emit the successive difference `value - previous` (quiet on the first).
    fn difference(&self) -> Stream {
        Stream(self.0.difference())
    }

    /// Negate each value (arithmetic `__neg__`, e.g. `5 -> -5`). Exposed as
    /// `not` (a Python keyword, so reach it with `getattr(stream, "not")()`).
    #[pyo3(name = "not")]
    fn not_(&self) -> Stream {
        Stream(self.0.not_())
    }

    /// Observe each value with a Python callable, passing it through unchanged;
    /// a raised exception aborts the run.
    fn inspect(&self, func: Py<PyAny>) -> Stream {
        Stream(self.0.inspect(func))
    }

    /// Collect every emitted value into a growing `list`, re-emitted each tick.
    fn accumulate(&self) -> Stream {
        Stream(self.0.accumulate())
    }

    /// Flush a `list` once `capacity` values accumulate (and on the last cycle).
    fn buffer(&self, capacity: usize) -> Stream {
        Stream(self.0.buffer(capacity))
    }

    /// Flush a `list` on each `interval_nanos` boundary (and on the last cycle).
    fn window(&self, interval_nanos: u64) -> Stream {
        Stream(self.0.window(Duration::from_nanos(interval_nanos)))
    }

    /// Pair each value with the engine time as a `(nanos, value)` tuple.
    fn with_time(&self) -> Stream {
        Stream(self.0.with_time())
    }

    /// Collect every `(nanos, value)` pair into a growing `list` of tuples.
    fn collect(&self) -> Stream {
        Stream(self.0.collect())
    }

    /// Fold values into an accumulator with `func(acc, value)`, seeded from
    /// `init`, emitting the accumulator after each fold. A raised exception
    /// aborts the run.
    fn fold(&self, init: Py<PyAny>, func: Py<PyAny>) -> Stream {
        Stream(self.0.fold(PyElement::from(init), func))
    }

    /// Map-and-filter: `func(value)` returning `None` drops the tick, any other
    /// result is emitted. A raised exception aborts the run.
    fn filter_map(&self, func: Py<PyAny>) -> Stream {
        Stream(self.0.filter_map(func))
    }

    /// Keep a value only when `predicate(value)` is truthy; drop it otherwise.
    fn filter_value(&self, predicate: Py<PyAny>) -> Stream {
        Stream(self.0.filter_value(predicate))
    }

    /// Drop values whose payload is Python `None`.
    fn filter_none(&self) -> Stream {
        Stream(self.0.filter_none())
    }

    /// Cumulative running sum over the numeric values.
    fn sum(&self) -> Stream {
        Stream(self.0.sum())
    }

    /// Cumulative running mean over the numeric values.
    fn mean(&self) -> Stream {
        Stream(self.0.mean())
    }

    /// Cumulative running mean over the numeric values (alias for `mean`, the
    /// classic method name).
    fn average(&self) -> Stream {
        Stream(self.0.mean())
    }

    /// Combine with `other` through `func(this_value, other_value)`, called
    /// whenever either input ticks (the classic `bimap`). A raised exception
    /// aborts the run.
    fn bimap(&self, other: PyRef<'_, Stream>, func: Py<PyAny>) -> Stream {
        Stream(self.0.bimap(&other.0, func))
    }

    /// The current value after the owning [`Graph`] has run.
    fn value(&self) -> Py<PyAny> {
        self.0.value().value()
    }
}

// Two demonstration ops authored the way a third-party crate would, proving the
// "author an op in Rust, call it from Python" path end to end.

// `scale` — the lightweight `pyop_fn!` form: an inline step, no `Op` struct.
crate::pyop_fn! {
    /// Multiply each value by `factor`.
    fn scale(factor: f64): f64 => f64 = |cfg, _state, a, _ctx| Ok(Tick::Value(*a * *cfg))
}

// `square` — the `#[pyop]` proc-macro form over a real `Op` impl. `#[pyop]`
// reads `In`/`Out`/`Cfg`/`State`/`cycle` off the impl and generates the same
// `#[pyfunction]` `pyop_fn!` writes by hand.
struct Square;

#[pyop(name = square)]
impl Op for Square {
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
        Ok(Tick::Value(input.0 * input.0))
    }
}

/// The `wingfoil_next` Python module.
#[pymodule]
fn wingfoil_next(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Graph>()?;
    m.add_class::<Stream>()?;
    m.add_function(wrap_pyfunction!(scale, m)?)?;
    m.add_function(wrap_pyfunction!(square, m)?)?;
    Ok(())
}
