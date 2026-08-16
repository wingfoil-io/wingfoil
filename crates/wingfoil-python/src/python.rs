//! `#[pyclass]` glue: the Python-facing `Graph` and `Stream` classes and the
//! `wingfoil` module.
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

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::graph::{PyGraph, PyStream};
use crate::statistics::{
    Aggregate, Moment, PyEwmaSpan, PyWeighting, PyWindow, py_aggregate, py_ewma, py_median,
    py_moment,
};
use crate::{Activation, Ctx, Op, PyElement, Tick, pyadapter, pygraph, pyop};

/// Map an `anyhow::Error` to a Python exception, preserving the whole context
/// chain (`{:#}`).
///
/// Public because `#[pyadapter]`-generated code names it: an adapter whose
/// wiring is fallible (`Result<Stream<T>>`) turns that error into a Python
/// exception at the seam, in this crate and in third-party op/adapter crates
/// alike.
pub fn to_pyerr(err: anyhow::Error) -> PyErr {
    PyRuntimeError::new_err(format!("{err:#}"))
}

/// A held wingfoil graph. Build sources on it, wire them with
/// [`Stream`] combinators, then [`run`](Graph::run).
#[pyclass(name = "Graph", unsendable)]
pub struct Graph(PyGraph);

impl Graph {
    /// The underlying erased object form — the seam a `#[pyadapter]` source
    /// `#[pyfunction]` in any crate uses to reach the builder and erase its
    /// result.
    pub fn object(&self) -> &PyGraph {
        &self.0
    }
}

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

    /// A source that replays a finite list of values, one per tick,
    /// `period_nanos` apart (first at t=0). The way to feed real data into a
    /// graph from Python. A graph containing it is single-run.
    fn values(&self, values: Vec<Py<PyAny>>, period_nanos: u64) -> Stream {
        let elements: Vec<PyElement> = values.into_iter().map(PyElement::from).collect();
        Stream(self.0.values(elements, Duration::from_nanos(period_nanos)))
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

    /// Merge with several other streams at once; the earliest-supplied ticked
    /// input wins.
    fn merge_all(&self, others: Vec<PyRef<'_, Stream>>) -> Stream {
        let streams: Vec<PyStream> = others.iter().map(|s| s.0.clone()).collect();
        Stream(self.0.merge_all(&streams))
    }

    /// Re-emit each value `delay_nanos` nanoseconds later.
    fn delay(&self, delay_nanos: u64) -> Stream {
        Stream(self.0.delay(Duration::from_nanos(delay_nanos)))
    }

    /// Suppress consecutive duplicate values (emit on change only).
    fn distinct(&self) -> Stream {
        Stream(self.0.distinct())
    }

    /// Suppress ticks while `is_small(current, last_emitted)` is truthy. The
    /// first value always ticks, and the comparison is against the last value
    /// actually emitted, so a slow drift still eventually ticks.
    fn drop_small_change(&self, is_small: Py<PyAny>) -> Stream {
        Stream(self.0.drop_small_change(is_small))
    }

    /// Emit the running tick count `1, 2, 3, …`, ignoring the values.
    fn count(&self) -> Stream {
        Stream(self.0.count())
    }

    /// Pass through the first `limit` values, then stay quiet.
    fn limit(&self, limit: usize) -> Stream {
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

    /// Negate each value arithmetically: `-value`, i.e. Python `__neg__`.
    /// `5` becomes `-5`, `5.0` becomes `-5.0`.
    ///
    /// This is **not** a logical `not` and **not** a bitwise `~`. It was
    /// called `not` before 9.0.0, named after the Rust op it wires rather than
    /// what it does, which misled on exactly the inputs you would test it
    /// with: `True` becomes `-1` (an `int`), not `False`, and `5` becomes
    /// `-5`, not `-6`.
    ///
    /// If you wanted one of those instead:
    ///
    /// * logical negation — `stream.map(lambda v: not v)`
    /// * bitwise complement — `stream.map(lambda v: ~v)`
    fn neg(&self) -> Stream {
        Stream(self.0.neg())
    }

    /// Observe each value with a Python callable, passing it through unchanged;
    /// a raised exception aborts the run.
    fn inspect(&self, func: Py<PyAny>) -> Stream {
        Stream(self.0.inspect(func))
    }

    /// Print each value to stdout as it ticks, passing it through unchanged
    /// (the legacy `print` debug tap).
    fn print(&self) -> Stream {
        Stream(self.0.print())
    }

    /// Log each value (`"{time} {label} {value:?}"`) as it ticks, passing it
    /// through unchanged (the legacy `logged` debug tap). `level` is one of
    /// `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"` (case-insensitive),
    /// defaulting to `"info"`; wire up any `log` backend to see the output.
    #[pyo3(signature = (label, level = "info"))]
    fn logged(&self, label: &str, level: &str) -> PyResult<Stream> {
        let level = parse_log_level(level)?;
        Ok(Stream(self.0.logged(label, level)))
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

    /// Sum of this stream of numbers over `window` (cumulative by default).
    /// See `mean` for the accepted `window` forms.
    #[pyo3(signature = (window=None))]
    fn sum(&self, window: Option<&Bound<'_, PyAny>>) -> PyResult<Stream> {
        Ok(Stream(py_aggregate(&self.0, Aggregate::Sum, window)?))
    }

    /// Minimum of this stream of numbers over `window` (cumulative by default).
    /// See `mean` for the accepted `window` forms.
    #[pyo3(signature = (window=None))]
    fn min(&self, window: Option<&Bound<'_, PyAny>>) -> PyResult<Stream> {
        Ok(Stream(py_aggregate(&self.0, Aggregate::Min, window)?))
    }

    /// Maximum of this stream of numbers over `window` (cumulative by default).
    /// See `mean` for the accepted `window` forms.
    #[pyo3(signature = (window=None))]
    fn max(&self, window: Option<&Bound<'_, PyAny>>) -> PyResult<Stream> {
        Ok(Stream(py_aggregate(&self.0, Aggregate::Max, window)?))
    }

    /// Mean of this stream of numbers over `window`.
    ///
    /// Args:
    ///     window: `Window.count(n)`, `Window.seconds(s)`, `Window.unbounded()`,
    ///         a plain `int` (shorthand for a count window), or `None`
    ///         (cumulative — the default).
    ///     weighting: `Weighting.Count` / `"count"` (default) for the ordinary
    ///         arithmetic mean, or `Weighting.Time` / `"time"` to weight each
    ///         sample by how long it was in effect.
    ///
    /// Returns:
    ///     A Stream of floats.
    #[pyo3(signature = (window=None, weighting=None))]
    fn mean(
        &self,
        window: Option<&Bound<'_, PyAny>>,
        weighting: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Stream> {
        Ok(Stream(py_moment(&self.0, Moment::Mean, window, weighting)?))
    }

    /// Cumulative running mean over the numeric values (alias for `mean()`, the
    /// legacy method name).
    fn average(&self) -> Stream {
        Stream(self.0.mean())
    }

    /// Variance of this stream of numbers over `window`.
    ///
    /// `Weighting.Count` gives the sample variance (ddof = 1); `Weighting.Time`
    /// gives the time-weighted population variance. Yields `0.0` until enough
    /// data is present. See `mean` for the argument forms.
    #[pyo3(signature = (window=None, weighting=None))]
    fn variance(
        &self,
        window: Option<&Bound<'_, PyAny>>,
        weighting: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Stream> {
        Ok(Stream(py_moment(
            &self.0,
            Moment::Variance,
            window,
            weighting,
        )?))
    }

    /// Standard deviation over `window` — the square root of `variance` under
    /// the same weighting. See `mean` for the argument forms.
    #[pyo3(signature = (window=None, weighting=None))]
    fn std(
        &self,
        window: Option<&Bound<'_, PyAny>>,
        weighting: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Stream> {
        Ok(Stream(py_moment(&self.0, Moment::Std, window, weighting)?))
    }

    /// Median of this stream of numbers over `window`.
    ///
    /// `Weighting.Time` gives the time-weighted median (the value at which
    /// cumulative in-effect time crosses one half). Over an unbounded window
    /// this retains every sample, so memory grows with the stream. See `mean`
    /// for the argument forms.
    #[pyo3(signature = (window=None, weighting=None))]
    fn median(
        &self,
        window: Option<&Bound<'_, PyAny>>,
        weighting: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Stream> {
        Ok(Stream(py_median(&self.0, window, weighting)?))
    }

    /// Exponentially weighted moving average of this stream of numbers.
    ///
    /// Args:
    ///     span: `EwmaSpan.per_tick(alpha)` for a fixed smoothing factor
    ///         applied once per tick, `EwmaSpan.half_life(seconds)` to decay by
    ///         elapsed graph time, or a plain `float` (shorthand for
    ///         `per_tick`). The first sample seeds the average.
    ///
    /// Returns:
    ///     A Stream of floats.
    fn ewma(&self, span: &Bound<'_, PyAny>) -> PyResult<Stream> {
        Ok(Stream(py_ewma(&self.0, span)?))
    }

    /// Combine with `other` through `func(this_value, other_value)`, called
    /// whenever either input ticks (the legacy `bimap`). A raised exception
    /// aborts the run.
    fn bimap(&self, other: PyRef<'_, Stream>, func: Py<PyAny>) -> Stream {
        Stream(self.0.bimap(&other.0, func))
    }

    /// Reduce values with `func(acc, value)`, emitting the running result. The
    /// first value seeds the accumulator; a raised exception aborts the run.
    fn reduce(&self, func: Py<PyAny>) -> Stream {
        Stream(self.0.reduce(func))
    }

    /// Decompose a stream of 2-tuples into its two component streams.
    fn split(&self) -> (Stream, Stream) {
        let (a, b) = self.0.split();
        (Stream(a), Stream(b))
    }

    /// Build a pandas `DataFrame` (columns `time`, `value`) from every value and
    /// its engine time; the final value (after the run) is the frame. Requires
    /// pandas at run time.
    fn dataframe(&self) -> Stream {
        Stream(self.0.dataframe())
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

/// Square each value: `square(stream)` yields `x * x` per tick.
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

// `running_total` — a **stateful** `#[pyop]`: the accumulator lives in the op's
// `State` (an `f64`, `Default`-seeded to 0.0 and re-seeded on each run), proving
// the proc macro handles state, not just stateless transforms.
struct RunningTotal;

/// Cumulative sum: `running_total(stream)` yields the running total of every
/// value seen so far. The accumulator is engine-owned state, re-seeded per run.
#[pyop(name = running_total)]
impl Op for RunningTotal {
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

// `weighted_add` — a **two-input** `#[pyop]` (`In<'a> = (&'a f64, &'a f64)`):
// combines two streams, proving the proc macro handles the two-input shape and
// emits a `module.weighted_add(stream, other)` function.
struct WeightedAdd;

/// Add two streams: `weighted_add(stream, other)` yields `a + b` per tick.
#[pyop(name = weighted_add)]
impl Op for WeightedAdd {
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

// `blend3` — a **three-input** `#[pyop]` (`In<'a> = (&'a f64, &'a f64, &'a f64)`,
// the `join3` shape): proves the proc macro handles three inputs and emits a
// `module.blend3(stream, second, third)` function over the `wire_op3` seam.
struct Blend3;

/// Combine three streams: `blend3(stream, second, third)` yields
/// `a + b * 10 + c * 100` per tick.
#[pyop(name = blend3)]
impl Op for Blend3 {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a f64, &'a f64, &'a f64);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&f64, &f64, &f64),
        _ctx: &mut Ctx<'_>,
    ) -> anyhow::Result<Tick<f64>> {
        Ok(Tick::Value(input.0 + input.1 * 10.0 + input.2 * 100.0))
    }
}

// `blend4` — a **four-input** `#[pyop]`: the widest arity the macro emits,
// over `wire_op4` / `Builder::register_op4`.
struct Blend4;

/// Combine four streams: `blend4(stream, second, third, fourth)` yields
/// `a + b * 10 + c * 100 + d * 1000` per tick.
#[pyop(name = blend4)]
impl Op for Blend4 {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a f64, &'a f64, &'a f64, &'a f64);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&f64, &f64, &f64, &f64),
        _ctx: &mut Ctx<'_>,
    ) -> anyhow::Result<Tick<f64>> {
        Ok(Tick::Value(
            input.0 + input.1 * 10.0 + input.2 * 100.0 + input.3 * 1000.0,
        ))
    }
}

// `clamped_scale` — a **tuple-`Cfg`** `#[pyop]`: `arg = (factor, ceiling)` gives
// each element of `Cfg = (f64, f64)` its own named Python parameter, so the call
// reads `clamped_scale(stream, factor, ceiling)` rather than passing a tuple.
struct ClampedScale;

/// Scale and cap: `clamped_scale(stream, factor, ceiling)` yields
/// `min(x * factor, ceiling)` per tick.
#[pyop(name = clamped_scale, arg = (factor, ceiling))]
impl Op for ClampedScale {
    type Cfg = (f64, f64);
    type State = ();
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (f64, f64),
        _state: &mut (),
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> anyhow::Result<Tick<f64>> {
        let (factor, ceiling) = *cfg;
        Ok(Tick::Value((input.0 * factor).min(ceiling)))
    }
}

// `doubled_running_total` — a **`#[pygraph]`**: a whole Rust-authored sub-graph
// (double each value, then cumulative-sum) exposed as one Python callable that
// splices its nodes into the caller's graph. The interior runs at native `f64`;
// only the edge erases. The wiring fn names the fluent `Stream` fully-qualified
// to avoid clashing with the `Stream` pyclass in this module.
/// A Rust-authored sub-graph as one call: `doubled_running_total(stream)`
/// doubles each value and then cumulatively sums it, splicing both nodes into
/// the caller's graph.
#[pygraph(name = doubled_running_total)]
fn build_doubled_running_total(
    input: &::wingfoil::prelude::Stream<f64>,
) -> ::wingfoil::prelude::Stream<f64> {
    use ::wingfoil::adapters::statistics::StatisticsOps;
    use ::wingfoil::prelude::StreamOps;
    input.map(|x: &f64| x * 2.0).cumulative_sum()
}

// `spread_and_mid` — a **multi-input, multi-output `#[pygraph]`**: two streams
// in, two out. The tuple return erases element-wise, so Python receives a tuple
// of streams and can wire onward from each.
/// Two streams in, two out: `spread_and_mid(bid, ask)` returns
/// `(spread, mid)` — `ask - bid` and `(ask + bid) / 2`.
#[pygraph(name = spread_and_mid)]
fn build_spread_and_mid(
    bid: &::wingfoil::prelude::Stream<f64>,
    ask: &::wingfoil::prelude::Stream<f64>,
) -> (
    ::wingfoil::prelude::Stream<f64>,
    ::wingfoil::prelude::Stream<f64>,
) {
    use ::wingfoil::prelude::StreamOps;
    let spread = bid.join(ask, |b: &f64, a: &f64| a - b);
    let mid = bid.join(ask, |b: &f64, a: &f64| (a + b) / 2.0);
    (spread, mid)
}

// `ramp_source` — a **source `#[pyadapter]`**: a user-style adapter trait
// implemented on `GraphBuilder`, exposed as `module.ramp_source(graph, start,
// step)`. This synthetic source (no real IO) emits `start, start+step, …` as
// `f64` every tick; a real adapter would open a socket/consumer here instead.
// The fluent `Stream`/`GraphBuilder` are named fully-qualified to avoid the
// `Stream` pyclass clash in this module.
trait RampSourceOps {
    fn ramp_source(&self, start: f64, step: f64) -> ::wingfoil::prelude::Stream<f64>;
}

#[pyadapter(name = ramp_source, source)]
impl RampSourceOps for ::wingfoil::prelude::GraphBuilder {
    /// A synthetic ramp source: `ramp_source(graph, start, step)` ticks
    /// `start`, `start + step`, `start + 2 * step`, … as `float`.
    fn ramp_source(&self, start: f64, step: f64) -> ::wingfoil::prelude::Stream<f64> {
        use ::wingfoil::prelude::{SourceOps, StreamOps};
        self.ticker(Duration::from_nanos(100))
            .count()
            .map(move |n: &u64| start + step * ((*n - 1) as f64))
    }
}

// `list_sink` — a **sink `#[pyadapter]`**: a user-style adapter authored as a
// trait on the fluent `Stream<f64>`, exposed as `module.list_sink(stream,
// target)`. It appends each value to a Python list (a real sink would write to
// a socket/DB); it produces a `Stream<()>` terminal (Python `None`). A raised
// append aborts the run.
trait ListSinkOps {
    fn list_sink(&self, target: Py<PyAny>) -> ::wingfoil::prelude::Stream<()>;
}

#[pyadapter(name = list_sink)]
impl ListSinkOps for ::wingfoil::prelude::Stream<f64> {
    /// Append each value to a Python list: `list_sink(stream, target)`.
    /// Returns a terminal stream whose value is `None`; a raised `append`
    /// aborts the run.
    fn list_sink(&self, target: Py<PyAny>) -> ::wingfoil::prelude::Stream<()> {
        use ::wingfoil::prelude::StreamOps;
        self.for_each(move |v: &f64| {
            Python::attach(|py| {
                target
                    .bind(py)
                    .call_method1("append", (*v,))
                    .map_err(|err| anyhow::anyhow!("list_sink append raised: {err}"))?;
                Ok(())
            })
        })
    }
}

// `pair_source` — a **burst source `#[pyadapter]`**: emits a `Burst<f64>` of two
// values per instant (grouped by `combine`), exposed as `module.pair_source(
// graph)` yielding a Python **list** per tick. Demonstrates burst-shaped adapter
// erasure (`Burst<T>` -> Python `list`), the shape most real adapters use.
trait PairSourceOps {
    fn pair_source(&self) -> ::wingfoil::prelude::Stream<::wingfoil::Burst<f64>>;
}

#[pyadapter(name = pair_source, source)]
impl PairSourceOps for ::wingfoil::prelude::GraphBuilder {
    /// A burst-shaped source: `pair_source(graph)` ticks a **list** of two
    /// floats — `[n, n * 10]` — sharing one instant.
    fn pair_source(&self) -> ::wingfoil::prelude::Stream<::wingfoil::Burst<f64>> {
        use ::wingfoil::prelude::{SourceOps, StreamOps};
        let a = self
            .ticker(Duration::from_nanos(100))
            .count()
            .map(|n: &u64| *n as f64);
        let b = self
            .ticker(Duration::from_nanos(100))
            .count()
            .map(|n: &u64| *n as f64 * 10.0);
        self.combine(&[a, b])
    }
}

// `split_source` — a **tuple-returning source `#[pyadapter]`**: one wiring call
// producing two streams, exposed as `module.split_source(graph) -> (Stream,
// Stream)`. This is the `(data, status)` shape a live source with a
// connection-status stream has (`zmq_sub`), and the reason `#[pyadapter]`
// accepts a tuple return at all.
/// A tuple-returning source: `split_source(graph)` returns
/// `(values, even)` — the tick count as a `float`, and whether it is even —
/// the same shape a live source with a status stream has.
#[pyadapter(name = split_source, source)]
fn split_source_demo(
    g: &::wingfoil::prelude::GraphBuilder,
) -> (
    ::wingfoil::prelude::Stream<f64>,
    ::wingfoil::prelude::Stream<bool>,
) {
    use ::wingfoil::prelude::{SourceOps, StreamOps};
    let counted = g.ticker(Duration::from_secs(1)).count();
    let values = counted.map(|i: &u64| *i as f64);
    let even = counted.map(|i: &u64| i.is_multiple_of(2));
    (values, even)
}

// `burst_list_sink` — a **burst sink `#[pyadapter]`** on `Stream<Burst<f64>>`:
// appends each burst (as a Python list) to a target list. Its `typed_burst_input`
// rebuilds a multi-value burst from each Python list, so a burst source
// round-trips into it (`Burst` -> list -> `Burst`); a scalar Python stream
// arrives as single-element bursts.
trait BurstListSinkOps {
    fn burst_list_sink(&self, target: Py<PyAny>) -> ::wingfoil::prelude::Stream<()>;
}

#[pyadapter(name = burst_list_sink)]
impl BurstListSinkOps for ::wingfoil::prelude::Stream<::wingfoil::Burst<f64>> {
    /// Append each **burst** to a Python list: `burst_list_sink(stream,
    /// target)` appends one list per tick. A scalar stream arrives as
    /// single-element bursts.
    fn burst_list_sink(&self, target: Py<PyAny>) -> ::wingfoil::prelude::Stream<()> {
        use ::wingfoil::prelude::StreamOps;
        self.for_each(move |burst: &::wingfoil::Burst<f64>| {
            Python::attach(|py| {
                let items: Vec<f64> = burst.iter().copied().collect();
                target
                    .bind(py)
                    .call_method1("append", (items,))
                    .map_err(|err| anyhow::anyhow!("burst_list_sink append raised: {err}"))?;
                Ok(())
            })
        })
    }
}

/// Outer-join several already-run streams on engine time into a single pandas
/// `DataFrame` — the multi-stream counterpart of [`Stream::dataframe`], and the
/// replacement for legacy's `wingfoil.pandas_helpers.build_dataframe`.
///
/// `streams` maps a column name to a stream that has already been run. Each
/// stream contributes one column named by its key, indexed by the times it
/// ticked; times where a stream was quiet come back as `NaN`. Column order
/// follows the dict's insertion order and the joined `time` is the leading
/// column.
///
/// A stream holds its history either as a frame (`stream.dataframe()`) or as
/// `(time, value)` tuples (`stream.collect()`) — both are accepted, so the join
/// composes with whichever half of the pair the caller already had. Streams that
/// produced nothing are skipped; if none produced anything the result is an
/// empty `DataFrame`. Requires pandas at call time.
#[pyfunction]
fn build_dataframe(streams: &Bound<'_, pyo3::types::PyDict>) -> PyResult<Py<PyAny>> {
    use pyo3::types::PyDictMethods;

    let mut columns = Vec::with_capacity(streams.len());
    for (key, value) in streams.iter() {
        let name: String = key.extract()?;
        let stream: PyRef<'_, Stream> = value.extract().map_err(|_| {
            PyValueError::new_err(format!(
                "build_dataframe() value for {name:?} is not a wingfoil Stream"
            ))
        })?;
        columns.push((name, stream.0.value()));
    }
    crate::graph::build_dataframe(&columns).map_err(to_pyerr)
}

/// Parse a case-insensitive level name into a [`log::Level`] for
/// [`Stream::logged`]. A ValueError names the accepted set on a bad input.
fn parse_log_level(level: &str) -> PyResult<log::Level> {
    match level.to_ascii_lowercase().as_str() {
        "trace" => Ok(log::Level::Trace),
        "debug" => Ok(log::Level::Debug),
        "info" => Ok(log::Level::Info),
        "warn" | "warning" => Ok(log::Level::Warn),
        "error" => Ok(log::Level::Error),
        other => Err(PyValueError::new_err(format!(
            "unknown log level {other:?}; expected one of \
             trace, debug, info, warn, error"
        ))),
    }
}

/// The `wingfoil` Python module.
/// The compiled extension, imported as the private `wingfoil._wingfoil`;
/// the `wingfoil` package under `python/` re-exports it.
#[pymodule]
fn _wingfoil(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Graph>()?;
    m.add_class::<Stream>()?;
    // The statistics argument objects: `Window` / `Weighting` / `EwmaSpan`
    // parameterise the `Stream` moment methods above.
    m.add_class::<PyWindow>()?;
    m.add_class::<PyWeighting>()?;
    m.add_class::<PyEwmaSpan>()?;
    m.add_function(wrap_pyfunction!(scale, m)?)?;
    m.add_function(wrap_pyfunction!(square, m)?)?;
    m.add_function(wrap_pyfunction!(running_total, m)?)?;
    m.add_function(wrap_pyfunction!(weighted_add, m)?)?;
    m.add_function(wrap_pyfunction!(blend3, m)?)?;
    m.add_function(wrap_pyfunction!(blend4, m)?)?;
    m.add_function(wrap_pyfunction!(clamped_scale, m)?)?;
    m.add_function(wrap_pyfunction!(doubled_running_total, m)?)?;
    m.add_function(wrap_pyfunction!(spread_and_mid, m)?)?;
    m.add_function(wrap_pyfunction!(crate::island::compiled_island, m)?)?;
    m.add_function(wrap_pyfunction!(crate::island::interpreted_twin, m)?)?;
    m.add_function(wrap_pyfunction!(ramp_source, m)?)?;
    m.add_function(wrap_pyfunction!(list_sink, m)?)?;
    m.add_function(wrap_pyfunction!(pair_source, m)?)?;
    m.add_function(wrap_pyfunction!(burst_list_sink, m)?)?;
    m.add_function(wrap_pyfunction!(split_source, m)?)?;
    m.add_function(wrap_pyfunction!(build_dataframe, m)?)?;
    register_latency(m)?;
    register_adapters(m)?;
    Ok(())
}

/// Register the latency surface (see [`crate::latency`]). Unconditional — the
/// engine's `latency` module is not feature-gated, so every wheel has it.
fn register_latency(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Imported by name for the same reason the adapter registrations are: the
    // hidden wrapper `wrap_pyfunction!` resolves is defined beside each
    // `#[pyfunction]`, so a module-qualified path does not resolve.
    use crate::latency::{
        PyLatency, PyLatencyStats, PyTracedBytes, latency_report, latency_report_if, stamp,
        stamp_if, stamp_precise, stamp_precise_if,
    };
    m.add_class::<PyLatency>()?;
    m.add_class::<PyTracedBytes>()?;
    m.add_class::<PyLatencyStats>()?;
    m.add_function(wrap_pyfunction!(stamp, m)?)?;
    m.add_function(wrap_pyfunction!(stamp_if, m)?)?;
    m.add_function(wrap_pyfunction!(stamp_precise, m)?)?;
    m.add_function(wrap_pyfunction!(stamp_precise_if, m)?)?;
    m.add_function(wrap_pyfunction!(latency_report, m)?)?;
    m.add_function(wrap_pyfunction!(latency_report_if, m)?)?;
    Ok(())
}

/// Register the per-adapter bindings the wheel was built with. Each adapter is
/// behind its own cargo feature (see `crate::adapters`), so a wheel built
/// without it simply has no such attribute.
fn register_adapters(m: &Bound<'_, PyModule>) -> PyResult<()> {
    #[cfg(feature = "postgres")]
    {
        // Imported by name (rather than called through the module path) because
        // `wrap_pyfunction!` resolves the hidden wrapper pyo3 defines alongside
        // each `#[pyfunction]`, which needs the name in scope.
        use crate::adapters::postgres::{
            postgres_notify_trigger_sql, postgres_read, postgres_source, postgres_sub,
            postgres_write,
        };
        m.add_function(wrap_pyfunction!(postgres_read, m)?)?;
        m.add_function(wrap_pyfunction!(postgres_sub, m)?)?;
        m.add_function(wrap_pyfunction!(postgres_source, m)?)?;
        m.add_function(wrap_pyfunction!(postgres_write, m)?)?;
        m.add_function(wrap_pyfunction!(postgres_notify_trigger_sql, m)?)?;
    }
    #[cfg(feature = "kafka")]
    {
        use crate::adapters::kafka::{kafka_pub, kafka_sub};
        m.add_function(wrap_pyfunction!(kafka_sub, m)?)?;
        m.add_function(wrap_pyfunction!(kafka_pub, m)?)?;
    }
    #[cfg(feature = "redis")]
    {
        use crate::adapters::redis::{redis_pub, redis_stream_read, redis_stream_write, redis_sub};
        m.add_function(wrap_pyfunction!(redis_sub, m)?)?;
        m.add_function(wrap_pyfunction!(redis_pub, m)?)?;
        m.add_function(wrap_pyfunction!(redis_stream_read, m)?)?;
        m.add_function(wrap_pyfunction!(redis_stream_write, m)?)?;
    }
    #[cfg(feature = "etcd")]
    {
        use crate::adapters::etcd::{etcd_pub, etcd_sub};
        m.add_function(wrap_pyfunction!(etcd_sub, m)?)?;
        m.add_function(wrap_pyfunction!(etcd_pub, m)?)?;
    }
    #[cfg(feature = "fluvio")]
    {
        use crate::adapters::fluvio::{fluvio_pub, fluvio_sub};
        m.add_function(wrap_pyfunction!(fluvio_sub, m)?)?;
        m.add_function(wrap_pyfunction!(fluvio_pub, m)?)?;
    }
    #[cfg(feature = "csv")]
    {
        use crate::adapters::csv::{csv_read, csv_write};
        m.add_function(wrap_pyfunction!(csv_read, m)?)?;
        m.add_function(wrap_pyfunction!(csv_write, m)?)?;
    }
    #[cfg(feature = "augurs")]
    {
        use crate::adapters::augurs::{
            augurs_changepoint, augurs_cluster, augurs_dtw, augurs_forecast, augurs_outlier,
            augurs_seasons,
        };
        m.add_function(wrap_pyfunction!(augurs_forecast, m)?)?;
        m.add_function(wrap_pyfunction!(augurs_changepoint, m)?)?;
        m.add_function(wrap_pyfunction!(augurs_seasons, m)?)?;
        m.add_function(wrap_pyfunction!(augurs_outlier, m)?)?;
        m.add_function(wrap_pyfunction!(augurs_dtw, m)?)?;
        m.add_function(wrap_pyfunction!(augurs_cluster, m)?)?;
    }
    #[cfg(feature = "kdb")]
    {
        use crate::adapters::kdb::{kdb_read, kdb_sub, kdb_write};
        m.add_function(wrap_pyfunction!(kdb_read, m)?)?;
        m.add_function(wrap_pyfunction!(kdb_sub, m)?)?;
        m.add_function(wrap_pyfunction!(kdb_write, m)?)?;
    }
    #[cfg(feature = "fix")]
    {
        use crate::adapters::fix::{
            PyFixConnection, fix_accept, fix_connect, fix_connect_tls, fix_send,
        };
        m.add_class::<PyFixConnection>()?;
        m.add_function(wrap_pyfunction!(fix_connect, m)?)?;
        m.add_function(wrap_pyfunction!(fix_accept, m)?)?;
        m.add_function(wrap_pyfunction!(fix_send, m)?)?;
        m.add_function(wrap_pyfunction!(fix_connect_tls, m)?)?;
    }
    #[cfg(feature = "prometheus")]
    {
        use crate::adapters::prometheus::PyPrometheusExporter;
        m.add_class::<PyPrometheusExporter>()?;
    }
    #[cfg(feature = "web")]
    {
        use crate::adapters::web::PyWebServer;
        m.add_class::<PyWebServer>()?;
    }
    #[cfg(feature = "ws")]
    {
        use crate::adapters::ws::{PyWsConnection, ws_sub};
        m.add_function(wrap_pyfunction!(ws_sub, m)?)?;
        m.add_class::<PyWsConnection>()?;
    }
    #[cfg(feature = "aeron")]
    {
        use crate::adapters::aeron::{
            aeron_pub, aeron_pub_with_status, aeron_sub, aeron_sub_with_status,
        };
        m.add_function(wrap_pyfunction!(aeron_sub, m)?)?;
        m.add_function(wrap_pyfunction!(aeron_sub_with_status, m)?)?;
        m.add_function(wrap_pyfunction!(aeron_pub, m)?)?;
        m.add_function(wrap_pyfunction!(aeron_pub_with_status, m)?)?;
    }
    #[cfg(feature = "iceoryx2")]
    {
        use crate::adapters::iceoryx2::{iceoryx2_pub, iceoryx2_sub};
        m.add_function(wrap_pyfunction!(iceoryx2_sub, m)?)?;
        m.add_function(wrap_pyfunction!(iceoryx2_pub, m)?)?;
    }
    #[cfg(feature = "otlp")]
    {
        use crate::adapters::otlp::otlp_push;
        m.add_function(wrap_pyfunction!(otlp_push, m)?)?;
    }
    #[cfg(feature = "zmq")]
    {
        use crate::adapters::zmq::{zmq_pub, zmq_sub};
        m.add_function(wrap_pyfunction!(zmq_sub, m)?)?;
        m.add_function(wrap_pyfunction!(zmq_pub, m)?)?;
        #[cfg(feature = "etcd")]
        {
            use crate::adapters::zmq::{zmq_pub_etcd, zmq_sub_etcd};
            m.add_function(wrap_pyfunction!(zmq_sub_etcd, m)?)?;
            m.add_function(wrap_pyfunction!(zmq_pub_etcd, m)?)?;
        }
    }
    // `m` is unused when no adapter feature is on.
    let _ = m;
    Ok(())
}
