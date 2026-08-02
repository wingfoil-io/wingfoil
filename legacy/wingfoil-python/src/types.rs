use anyhow::anyhow;
use pyo3::exceptions::{PyException, PyTypeError, PyValueError};
use std::time::SystemTime;

use ::wingfoil::{NanoTime, RunFor, RunMode};

use pyo3::prelude::*;

use std::time::Duration;
use std::time::UNIX_EPOCH;

pub trait ToPyResult<T> {
    fn to_pyresult(self) -> PyResult<T>;
}

/// `expect` message for inserting into a freshly-created `PyDict`. Inserting
/// hashable str/bytes/primitive keys into a new dict cannot fail.
pub const DICT_INSERT_INFALLIBLE: &str = "invariant: inserting into a fresh dict cannot fail";

/// `expect` message for constructing a `PyList` from an in-memory `Vec`.
pub const LIST_NEW_INFALLIBLE: &str = "invariant: PyList::new from a Vec cannot fail";

/// `expect` message for `IntoPyObject` on primitive / `String` values whose
/// conversion is infallible.
pub const INTO_PY_INFALLIBLE: &str =
    "invariant: IntoPyObject for this primitive type is infallible";

/// Typed binding errors that map to specific Python exception classes when
/// surfaced through [`ToPyResult`]. Construct via the helpers
/// [`py_type_error`] / [`py_value_error`] so the variant rides inside the
/// `anyhow::Error` and is downcast at the `#[pymethods]` boundary.
#[derive(Debug, thiserror::Error)]
pub enum PyBindingError {
    #[error("{0}")]
    TypeError(String),
    #[error("{0}")]
    ValueError(String),
}

/// Build an `anyhow::Error` that surfaces as a Python `TypeError`.
pub fn py_type_error(msg: impl Into<String>) -> anyhow::Error {
    anyhow!(PyBindingError::TypeError(msg.into()))
}

/// Build an `anyhow::Error` that surfaces as a Python `ValueError`.
pub fn py_value_error(msg: impl Into<String>) -> anyhow::Error {
    anyhow!(PyBindingError::ValueError(msg.into()))
}

impl<T> ToPyResult<T> for anyhow::Result<T> {
    fn to_pyresult(self) -> PyResult<T> {
        self.map_err(|e| {
            // Preserve a Python exception raised inside a user callback so the
            // original type / message / traceback propagate unchanged.
            if let Some(py_err) = e.downcast_ref::<PyErr>() {
                return Python::attach(|py| py_err.clone_ref(py));
            }
            match e.downcast_ref::<PyBindingError>() {
                Some(PyBindingError::TypeError(msg)) => PyTypeError::new_err(msg.clone()),
                Some(PyBindingError::ValueError(msg)) => PyValueError::new_err(msg.clone()),
                None => PyException::new_err(e.to_string()),
            }
        })
    }
}

pub fn parse_run_args(
    py: Python<'_>,
    realtime: bool,
    start: Option<Py<PyAny>>,
    duration: Option<Py<PyAny>>,
    cycles: Option<u32>,
) -> anyhow::Result<(RunMode, RunFor)> {
    if duration.is_some() && cycles.is_some() {
        anyhow::bail!(py_value_error("Cannot specify both duration and cycles"));
    }
    if realtime && start.is_some() {
        anyhow::bail!(py_value_error("Cannot specify start in realtime mode"));
    }
    let run_mode = if realtime {
        RunMode::RealTime
    } else {
        let t = match start {
            Some(start) => to_nano_time(py, start)?,
            None => NanoTime::ZERO,
        };
        RunMode::HistoricalFrom(t)
    };
    let run_for = if let Some(cycles) = cycles {
        RunFor::Cycles(cycles)
    } else {
        match duration {
            Some(duration) => {
                let duration = to_duration(py, duration)?;
                RunFor::Duration(duration)
            }
            None => RunFor::Forever,
        }
    };
    Ok((run_mode, run_for))
}

fn to_duration(py: Python<'_>, obj: Py<PyAny>) -> anyhow::Result<Duration> {
    if let Ok(f) = obj.extract::<f64>(py) {
        if f < 0.0 {
            anyhow::bail!("duration can not be negative");
        }
        let nanos = (f * 1e9) as u64;
        Ok(Duration::from_nanos(nanos))
    } else if let Ok(td) = obj.extract::<Duration>(py) {
        Ok(td)
    } else {
        anyhow::bail!("failed to convert duration");
    }
}

fn f64_secs_to_nanos(ts: f64) -> anyhow::Result<u64> {
    if ts.is_sign_negative() {
        return Err(anyhow!("Negative timestamps not supported"));
    }
    let secs = ts.trunc() as u64;
    let frac_nanos = (ts.fract() * 1e9) as u64;
    let nanos = secs
        .checked_mul(1_000_000_000)
        .ok_or_else(|| anyhow!("Overflow when converting seconds to nanoseconds"))?;
    let total_nanos = nanos
        .checked_add(frac_nanos)
        .ok_or_else(|| anyhow!("Overflow when adding fractional nanoseconds"))?;
    Ok(total_nanos)
}

fn to_nano_time(py: Python<'_>, obj: Py<PyAny>) -> anyhow::Result<NanoTime> {
    if let Ok(dt) = obj.extract::<SystemTime>(py) {
        let nanos = dt.duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(nanos.into())
    } else if let Ok(ts) = obj.extract::<f64>(py) {
        Ok(f64_secs_to_nanos(ts)?.into())
    } else {
        Err(anyhow!("failed to convert to NanoTime"))
    }
}
