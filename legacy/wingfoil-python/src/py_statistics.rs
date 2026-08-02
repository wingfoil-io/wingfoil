//! Python bindings for the statistics adapter.
//!
//! The statistics adapter is pure Rust with no external service, so — like
//! augurs — every binding is a stream transform exposed as a method on
//! [`PyStream`](crate::py_stream::PyStream) rather than a source function.
//! Each consumes a stream of numbers and emits a stream of floats:
//!
//! - `.mean(window, weighting)` / `.variance(...)` / `.std(...)`
//! - `.sum(window)` / `.min(window)` / `.max(window)` / `.median(window, weighting)`
//! - `.ewma(span)`
//!
//! The Rust API takes a [`Window`] and a [`Weighting`] by value; Python gets
//! the same two knobs through the [`Window`](PyWindow) and
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

use std::rc::Rc;
use std::time::Duration;

use pyo3::prelude::*;
use wingfoil::Stream;
use wingfoil::adapters::statistics::{EwmaSpan, StatisticsOperators, Weighting, Window};

use crate::py_element::PyElement;
use crate::py_stream::as_floats;

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

/// How an [`ewma`](crate::py_stream::PyStream::ewma) decays older observations.
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
        // The Rust operator only `debug_assert!`s this, so a release build
        // would silently produce a diverging (or frozen) average. Reject it at
        // the boundary instead.
        if !(0.0..=1.0).contains(&alpha) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
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
            return Err(pyo3::exceptions::PyValueError::new_err(
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
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
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
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
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
            other => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{op}: unknown weighting '{other}' (expected 'count' or 'time')"
            ))),
        };
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "{op}: weighting must be a Weighting, 'count', 'time', or None"
    )))
}

/// Resolve the `span` argument of [`py_ewma_inner`].
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
    Err(pyo3::exceptions::PyTypeError::new_err(
        "ewma: span must be an EwmaSpan or a float (per-tick alpha)",
    ))
}

/// A moment operator selected by the `.mean()` / `.variance()` / `.std()`
/// stream methods, which differ only in which trait method they call.
#[derive(Clone, Copy)]
pub enum PyMoment {
    Mean,
    Variance,
    Std,
}

impl PyMoment {
    fn name(self) -> &'static str {
        match self {
            PyMoment::Mean => "mean",
            PyMoment::Variance => "variance",
            PyMoment::Std => "std",
        }
    }
}

/// Inner implementation for the `.mean()` / `.variance()` / `.std()` stream
/// methods.
pub fn py_moment_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    moment: PyMoment,
    window: Option<&Bound<'_, PyAny>>,
    weighting: Option<&Bound<'_, PyAny>>,
) -> PyResult<Rc<dyn Stream<f64>>> {
    let op = moment.name();
    let window = window_from_py(op, window)?;
    let weighting = weighting_from_py(op, weighting)?;
    let floats = as_floats(stream, op);
    Ok(match moment {
        PyMoment::Mean => floats.mean(window, weighting),
        PyMoment::Variance => floats.variance(window, weighting),
        PyMoment::Std => floats.std(window, weighting),
    })
}

/// An unweighted window operator selected by the `.sum()` / `.min()` /
/// `.max()` stream methods.
#[derive(Clone, Copy)]
pub enum PyAggregate {
    Sum,
    Min,
    Max,
}

impl PyAggregate {
    fn name(self) -> &'static str {
        match self {
            PyAggregate::Sum => "sum",
            PyAggregate::Min => "min",
            PyAggregate::Max => "max",
        }
    }
}

/// Inner implementation for the `.sum()` / `.min()` / `.max()` stream methods.
pub fn py_aggregate_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    aggregate: PyAggregate,
    window: Option<&Bound<'_, PyAny>>,
) -> PyResult<Rc<dyn Stream<f64>>> {
    let op = aggregate.name();
    let window = window_from_py(op, window)?;
    let floats = as_floats(stream, op);
    Ok(match aggregate {
        PyAggregate::Sum => floats.sum(window),
        PyAggregate::Min => floats.min(window),
        PyAggregate::Max => floats.max(window),
    })
}

/// Inner implementation for the `.median()` stream method.
pub fn py_median_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: Option<&Bound<'_, PyAny>>,
    weighting: Option<&Bound<'_, PyAny>>,
) -> PyResult<Rc<dyn Stream<f64>>> {
    let window = window_from_py("median", window)?;
    let weighting = weighting_from_py("median", weighting)?;
    Ok(as_floats(stream, "median").median(window, weighting))
}

/// Inner implementation for the `.ewma()` stream method.
pub fn py_ewma_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    span: &Bound<'_, PyAny>,
) -> PyResult<Rc<dyn Stream<f64>>> {
    let span = span_from_py(span)?;
    Ok(as_floats(stream, "ewma").ewma(span))
}
