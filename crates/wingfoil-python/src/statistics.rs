//! Python bindings for the statistics ops (`wingfoil::adapters::statistics`).
//!
//! The statistics layer is pure Rust with no external service, so — like the
//! augurs bindings — every binding is a stream transform exposed as a method on
//! [`Stream`](crate::Stream) rather than a source function. Each consumes a
//! stream of numbers and emits a stream of floats:
//!
//! - `.mean(window, weighting)` / `.variance(...)` / `.std(...)`
//! - `.sum(window)` / `.min(window)` / `.max(window)` / `.median(window, weighting)`
//! - `.ewma(span)`
//!
//! **Why a `Window` object rather than one method per shape.** The engine's
//! [`StatisticsOps`] spells every combination out as its own method
//! (`rolling_mean`, `time_windowed_mean`, `cumulative_mean_time_weighted`, …)
//! because Rust can afford a wide, statically-checked surface. Python cannot:
//! the legacy binding settled on two orthogonal knobs — a [`Window`] and a
//! [`Weighting`] — and that is the contract wingfoil must be a superset of. So this
//! module is a **dispatcher**: it resolves the two knobs from Python, then picks
//! the one engine method that implements that pair. Nothing here computes a
//! statistic.
//!
//! Python gets the two knobs through the [`Window`](PyWindow) and
//! [`Weighting`](PyWeighting) classes, with `int` and `str` shorthands so the
//! common cases stay terse:
//!
//! ```python
//! from wingfoil import Window, Weighting, EwmaSpan
//!
//! prices.mean()                                  # cumulative, count weighted
//! prices.mean(10)                                # last 10 samples
//! prices.mean(Window.seconds(5.0), "time")       # 5s of graph time, time weighted
//! prices.ewma(EwmaSpan.half_life(30.0))
//! ```
//!
//! A bare `float` is deliberately *not* accepted as a window: `mean(10)` and
//! `mean(10.0)` would mean wildly different things (ten samples vs ten
//! seconds), so a time window must say so via `Window.seconds(...)`.

use std::time::Duration;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use wingfoil::adapters::statistics::StatisticsOps;
use wingfoil::ops::EwmaDecay;

use crate::graph::PyStream;

/// The extent an operator aggregates over — the resolved form of
/// [`Window`](PyWindow).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Window {
    /// The most recent `n` samples. The engine clamps a zero-sample window to
    /// one, so `Window.count(0)` behaves as `Window.count(1)`.
    Count(usize),
    /// Every sample seen in the last `Duration` of graph time (a sample exactly
    /// that old is still in the window).
    Time(Duration),
    /// Every sample seen so far — a cumulative (expanding) window.
    Unbounded,
}

/// How samples are weighted when aggregating — the resolved form of
/// [`Weighting`](PyWeighting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weighting {
    /// Every sample counts equally — the ordinary arithmetic statistic.
    Count,
    /// Each sample is weighted by how long it was in effect.
    Time,
}

/// How samples are weighted when aggregating a stream.
#[pyclass(eq, eq_int, name = "Weighting", from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyWeighting {
    /// Every sample counts equally — the ordinary arithmetic statistic.
    Count,
    /// Each sample is weighted by how long it was in effect (elapsed graph
    /// time since the previous sample).
    Time,
}

impl From<PyWeighting> for Weighting {
    fn from(w: PyWeighting) -> Self {
        match w {
            PyWeighting::Count => Weighting::Count,
            PyWeighting::Time => Weighting::Time,
        }
    }
}

/// The extent an operator aggregates over.
///
/// Build one with `Window.count(n)`, `Window.seconds(secs)` or
/// `Window.unbounded()`. Operators also accept a plain `int` as shorthand for
/// `Window.count(n)`, and `None` for `Window.unbounded()`.
#[pyclass(name = "Window", frozen, eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PyWindow(Window);

#[pymethods]
impl PyWindow {
    /// The most recent `n` samples (clamped to at least one).
    #[staticmethod]
    fn count(n: usize) -> Self {
        Self(Window::Count(n))
    }

    /// All samples seen in the last `seconds` of graph time.
    #[staticmethod]
    fn seconds(seconds: f64) -> PyResult<Self> {
        Ok(Self(Window::Time(duration_from_secs(
            "Window.seconds",
            seconds,
        )?)))
    }

    /// Every sample seen so far — a cumulative (expanding) window.
    #[staticmethod]
    fn unbounded() -> Self {
        Self(Window::Unbounded)
    }

    fn __repr__(&self) -> String {
        match self.0 {
            Window::Count(n) => format!("Window.count({n})"),
            Window::Time(d) => format!("Window.seconds({})", d.as_secs_f64()),
            Window::Unbounded => "Window.unbounded()".to_string(),
        }
    }
}

/// How an `ewma` decays older observations — the resolved form of
/// [`EwmaSpan`](PyEwmaSpan).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EwmaSpan {
    /// Fixed smoothing factor applied once per tick.
    PerTick(f64),
    /// Time decay: a sample's weight halves every `Duration` of elapsed graph
    /// time.
    HalfLife(Duration),
}

impl From<EwmaSpan> for EwmaDecay {
    fn from(span: EwmaSpan) -> Self {
        match span {
            EwmaSpan::PerTick(alpha) => EwmaDecay::PerTick(alpha),
            EwmaSpan::HalfLife(half_life) => EwmaDecay::HalfLife(half_life.as_nanos() as f64),
        }
    }
}

/// How the `ewma` stream method decays older observations.
///
/// Build one with `EwmaSpan.per_tick(alpha)` or `EwmaSpan.half_life(seconds)`.
/// `ewma` also accepts a plain `float` as shorthand for
/// `EwmaSpan.per_tick(alpha)`.
#[pyclass(name = "EwmaSpan", frozen, eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PyEwmaSpan(EwmaSpan);

#[pymethods]
impl PyEwmaSpan {
    /// Fixed smoothing factor applied once per tick
    /// (`ewma_t = alpha * x_t + (1 - alpha) * ewma_{t-1}`).
    #[staticmethod]
    fn per_tick(alpha: f64) -> PyResult<Self> {
        // The Rust op only `debug_assert!`s this, so a release build would
        // silently produce a diverging (or frozen) average. Reject it at the
        // boundary instead.
        if !(0.0..=1.0).contains(&alpha) {
            return Err(PyValueError::new_err(format!(
                "ewma: per_tick alpha must be between 0 and 1, got {alpha}"
            )));
        }
        Ok(Self(EwmaSpan::PerTick(alpha)))
    }

    /// Time decay: a sample's weight halves every `seconds` of elapsed graph
    /// time, independent of tick rate.
    #[staticmethod]
    fn half_life(seconds: f64) -> PyResult<Self> {
        let half_life = duration_from_secs("EwmaSpan.half_life", seconds)?;
        if half_life.is_zero() {
            return Err(PyValueError::new_err(
                "EwmaSpan.half_life: seconds must be greater than 0",
            ));
        }
        Ok(Self(EwmaSpan::HalfLife(half_life)))
    }

    fn __repr__(&self) -> String {
        match self.0 {
            EwmaSpan::PerTick(alpha) => format!("EwmaSpan.per_tick({alpha})"),
            EwmaSpan::HalfLife(d) => format!("EwmaSpan.half_life({})", d.as_secs_f64()),
        }
    }
}

/// Convert a seconds `float` to a [`Duration`], rejecting the negative and
/// non-finite values that [`Duration::from_secs_f64`] would panic on.
fn duration_from_secs(op: &str, seconds: f64) -> PyResult<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(PyValueError::new_err(format!(
            "{op}: seconds must be a finite, non-negative number, got {seconds}"
        )));
    }
    Ok(Duration::from_secs_f64(seconds))
}

/// Resolve the `window` argument shared by every statistics operator.
///
/// Accepts a [`Window`](PyWindow), a plain `int` (shorthand for a count
/// window), or `None` (an unbounded/cumulative window).
fn window_from_py(op: &str, window: Option<&Bound<'_, PyAny>>) -> PyResult<Window> {
    let Some(window) = window else {
        return Ok(Window::Unbounded);
    };
    if window.is_none() {
        return Ok(Window::Unbounded);
    }
    if let Ok(window) = window.extract::<PyWindow>() {
        return Ok(window.0);
    }
    // `extract::<usize>` goes through `__index__`, so a `float` lands in the
    // error below rather than being silently truncated to a count window.
    if let Ok(n) = window.extract::<usize>() {
        return Ok(Window::Count(n));
    }
    Err(PyTypeError::new_err(format!(
        "{op}: window must be a Window, an int (sample count), or None (unbounded) — \
         pass Window.seconds(...) for a time window"
    )))
}

/// Resolve the `weighting` argument of the moment operators.
///
/// Accepts a [`Weighting`](PyWeighting), the string `"count"` or `"time"`, or
/// `None` (count weighting).
fn weighting_from_py(op: &str, weighting: Option<&Bound<'_, PyAny>>) -> PyResult<Weighting> {
    let Some(weighting) = weighting else {
        return Ok(Weighting::Count);
    };
    if weighting.is_none() {
        return Ok(Weighting::Count);
    }
    if let Ok(weighting) = weighting.extract::<PyWeighting>() {
        return Ok(weighting.into());
    }
    if let Ok(name) = weighting.extract::<String>() {
        return match name.to_ascii_lowercase().as_str() {
            "count" => Ok(Weighting::Count),
            "time" => Ok(Weighting::Time),
            other => Err(PyValueError::new_err(format!(
                "{op}: unknown weighting '{other}' (expected 'count' or 'time')"
            ))),
        };
    }
    Err(PyTypeError::new_err(format!(
        "{op}: weighting must be a Weighting, 'count', 'time', or None"
    )))
}

/// Resolve the `span` argument of [`py_ewma`].
///
/// Accepts an [`EwmaSpan`](PyEwmaSpan) or a plain `float` (shorthand for a
/// per-tick smoothing factor).
fn span_from_py(span: &Bound<'_, PyAny>) -> PyResult<EwmaSpan> {
    if let Ok(span) = span.extract::<PyEwmaSpan>() {
        return Ok(span.0);
    }
    if let Ok(alpha) = span.extract::<f64>() {
        return Ok(PyEwmaSpan::per_tick(alpha)?.0);
    }
    Err(PyTypeError::new_err(
        "ewma: span must be an EwmaSpan or a float (per-tick alpha)",
    ))
}

/// A moment operator selected by the `.mean()` / `.variance()` / `.std()`
/// stream methods, which differ only in which engine method they reach.
#[derive(Clone, Copy)]
pub enum Moment {
    Mean,
    Variance,
    Std,
}

impl Moment {
    fn name(self) -> &'static str {
        match self {
            Moment::Mean => "mean",
            Moment::Variance => "variance",
            Moment::Std => "std",
        }
    }
}

/// An unweighted window operator selected by the `.sum()` / `.min()` /
/// `.max()` stream methods.
#[derive(Clone, Copy)]
pub enum Aggregate {
    Sum,
    Min,
    Max,
}

impl Aggregate {
    fn name(self) -> &'static str {
        match self {
            Aggregate::Sum => "sum",
            Aggregate::Min => "min",
            Aggregate::Max => "max",
        }
    }
}

/// Wire a moment operator: the (window, weighting) pair selects one engine
/// method from [`StatisticsOps`].
pub fn moment(stream: &PyStream, moment: Moment, window: Window, weighting: Weighting) -> PyStream {
    stream.wire_float_stat(moment.name(), move |s| match (moment, window, weighting) {
        (Moment::Mean, Window::Unbounded, Weighting::Count) => s.cumulative_mean(),
        (Moment::Mean, Window::Count(n), Weighting::Count) => s.rolling_mean(n),
        (Moment::Mean, Window::Time(d), Weighting::Count) => s.time_windowed_mean(d),
        (Moment::Mean, Window::Unbounded, Weighting::Time) => s.cumulative_mean_time_weighted(),
        (Moment::Mean, Window::Count(n), Weighting::Time) => s.rolling_mean_time_weighted(n),
        (Moment::Mean, Window::Time(d), Weighting::Time) => s.time_windowed_mean_time_weighted(d),
        (Moment::Variance, Window::Unbounded, Weighting::Count) => s.cumulative_var(),
        (Moment::Variance, Window::Count(n), Weighting::Count) => s.rolling_var(n),
        (Moment::Variance, Window::Time(d), Weighting::Count) => s.time_windowed_var(d),
        (Moment::Variance, Window::Unbounded, Weighting::Time) => s.cumulative_var_time_weighted(),
        (Moment::Variance, Window::Count(n), Weighting::Time) => s.rolling_var_time_weighted(n),
        (Moment::Variance, Window::Time(d), Weighting::Time) => {
            s.time_windowed_var_time_weighted(d)
        }
        (Moment::Std, Window::Unbounded, Weighting::Count) => s.cumulative_std(),
        (Moment::Std, Window::Count(n), Weighting::Count) => s.rolling_std(n),
        (Moment::Std, Window::Time(d), Weighting::Count) => s.time_windowed_std(d),
        (Moment::Std, Window::Unbounded, Weighting::Time) => s.cumulative_std_time_weighted(),
        (Moment::Std, Window::Count(n), Weighting::Time) => s.rolling_std_time_weighted(n),
        (Moment::Std, Window::Time(d), Weighting::Time) => s.time_windowed_std_time_weighted(d),
    })
}

/// Wire an unweighted aggregate: the window alone selects the engine method.
pub fn aggregate(stream: &PyStream, aggregate: Aggregate, window: Window) -> PyStream {
    stream.wire_float_stat(aggregate.name(), move |s| match (aggregate, window) {
        (Aggregate::Sum, Window::Unbounded) => s.cumulative_sum(),
        (Aggregate::Sum, Window::Count(n)) => s.rolling_sum(n),
        (Aggregate::Sum, Window::Time(d)) => s.time_windowed_sum(d),
        (Aggregate::Min, Window::Unbounded) => s.cumulative_min(),
        (Aggregate::Min, Window::Count(n)) => s.rolling_min(n),
        (Aggregate::Min, Window::Time(d)) => s.time_windowed_min(d),
        (Aggregate::Max, Window::Unbounded) => s.cumulative_max(),
        (Aggregate::Max, Window::Count(n)) => s.rolling_max(n),
        (Aggregate::Max, Window::Time(d)) => s.time_windowed_max(d),
    })
}

/// Wire a median: the (window, weighting) pair selects the engine method.
pub fn median(stream: &PyStream, window: Window, weighting: Weighting) -> PyStream {
    stream.wire_float_stat("median", move |s| match (window, weighting) {
        (Window::Unbounded, Weighting::Count) => s.cumulative_median(),
        (Window::Count(n), Weighting::Count) => s.rolling_median(n),
        (Window::Time(d), Weighting::Count) => s.time_windowed_median(d),
        (Window::Unbounded, Weighting::Time) => s.cumulative_median_time_weighted(),
        (Window::Count(n), Weighting::Time) => s.rolling_median_time_weighted(n),
        (Window::Time(d), Weighting::Time) => s.time_windowed_median_time_weighted(d),
    })
}

/// Wire an exponentially weighted moving average.
pub fn ewma(stream: &PyStream, span: EwmaSpan) -> PyStream {
    let decay = EwmaDecay::from(span);
    stream.wire_float_stat("ewma", move |s| s.ewma(decay))
}

/// The pyo3 entry point for `.mean()` / `.variance()` / `.std()`: resolve the
/// Python arguments, then dispatch.
pub fn py_moment(
    stream: &PyStream,
    kind: Moment,
    window: Option<&Bound<'_, PyAny>>,
    weighting: Option<&Bound<'_, PyAny>>,
) -> PyResult<PyStream> {
    let op = kind.name();
    let window = window_from_py(op, window)?;
    let weighting = weighting_from_py(op, weighting)?;
    Ok(moment(stream, kind, window, weighting))
}

/// The pyo3 entry point for `.sum()` / `.min()` / `.max()`.
pub fn py_aggregate(
    stream: &PyStream,
    kind: Aggregate,
    window: Option<&Bound<'_, PyAny>>,
) -> PyResult<PyStream> {
    let window = window_from_py(kind.name(), window)?;
    Ok(aggregate(stream, kind, window))
}

/// The pyo3 entry point for `.median()`.
pub fn py_median(
    stream: &PyStream,
    window: Option<&Bound<'_, PyAny>>,
    weighting: Option<&Bound<'_, PyAny>>,
) -> PyResult<PyStream> {
    let window = window_from_py("median", window)?;
    let weighting = weighting_from_py("median", weighting)?;
    Ok(median(stream, window, weighting))
}

/// The pyo3 entry point for `.ewma()`.
pub fn py_ewma(stream: &PyStream, span: &Bound<'_, PyAny>) -> PyResult<PyStream> {
    Ok(ewma(stream, span_from_py(span)?))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wingfoil::{NanoTime, RunFor, RunMode};

    use super::*;
    use crate::PyGraph;

    /// `1.0, 2.0, 3.0, …`, one per second of graph time (the legacy
    /// `ticker(1.0).count().map(float)` fixture).
    fn counts(g: &PyGraph) -> PyStream {
        g.counter(Duration::from_secs(1))
    }

    fn run_cycles(g: &PyGraph, cycles: u32) {
        g.run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(cycles),
        )
        .expect("invariant: the statistics fixture graph runs clean");
    }

    /// Every value `stream` emitted, read back after the run.
    fn emitted(stream: &PyStream) -> Vec<f64> {
        Python::attach(|py| {
            stream
                .value()
                .value()
                .extract::<Vec<f64>>(py)
                .expect("invariant: accumulate() of a float stat is a list of floats")
        })
    }

    #[test]
    fn window_dispatch_covers_count_time_and_unbounded() {
        let g = PyGraph::new();
        let cumulative = aggregate(&counts(&g), Aggregate::Sum, Window::Unbounded).accumulate();
        let rolling = aggregate(&counts(&g), Aggregate::Sum, Window::Count(3)).accumulate();
        let timed = aggregate(
            &counts(&g),
            Aggregate::Sum,
            Window::Time(Duration::from_secs(3)),
        )
        .accumulate();
        run_cycles(&g, 5);
        assert_eq!(vec![1.0, 3.0, 6.0, 10.0, 15.0], emitted(&cumulative));
        assert_eq!(vec![1.0, 3.0, 6.0, 9.0, 12.0], emitted(&rolling));
        assert_eq!(vec![1.0, 3.0, 6.0, 10.0, 14.0], emitted(&timed));
    }

    #[test]
    fn weighting_dispatch_selects_the_time_weighted_twin() {
        let g = PyGraph::new();
        let counted = moment(
            &counts(&g),
            Moment::Mean,
            Window::Unbounded,
            Weighting::Count,
        )
        .accumulate();
        let weighted = moment(
            &counts(&g),
            Moment::Mean,
            Window::Unbounded,
            Weighting::Time,
        )
        .accumulate();
        run_cycles(&g, 5);
        assert_eq!(vec![1.0, 1.5, 2.0, 2.5, 3.0], emitted(&counted));
        assert_eq!(vec![1.0, 1.0, 1.5, 2.0, 2.5], emitted(&weighted));
    }

    #[test]
    fn ewma_per_tick_matches_the_closed_form() {
        let g = PyGraph::new();
        let smoothed = ewma(&counts(&g), EwmaSpan::PerTick(0.5)).accumulate();
        run_cycles(&g, 5);
        assert_eq!(vec![1.0, 1.5, 2.25, 3.125, 4.0625], emitted(&smoothed));
    }

    #[test]
    fn median_dispatch_reaches_the_windowed_engine_op() {
        let g = PyGraph::new();
        let rolling = median(&counts(&g), Window::Count(4), Weighting::Count).accumulate();
        run_cycles(&g, 6);
        // An even window averages its two middle samples.
        assert_eq!(vec![1.0, 1.5, 2.0, 2.5, 3.5, 4.5], emitted(&rolling));
    }

    #[test]
    fn non_numeric_input_names_the_operator() {
        let lambda = Python::attach(|py| {
            py.eval(c"lambda n: 'x'", None, None)
                .expect("invariant: the test lambda compiles")
                .unbind()
        });
        let g = PyGraph::new();
        let _bad = median(&counts(&g).map(lambda), Window::Unbounded, Weighting::Count);
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .expect_err("a non-numeric value must abort the run");
        assert!(format!("{err:#}").contains("median: expected a float input value"));
    }
}
