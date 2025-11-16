use anyhow::anyhow;
use pyo3::exceptions::PyException;
use std::time::SystemTime;

use ::wingfoil::{NanoTime, RunFor, RunMode};

use pyo3::prelude::*;

use lazy_static::lazy_static;
use std::time::Duration;
use std::time::UNIX_EPOCH;

lazy_static! {
    pub static ref DUMMY_PY_ELEMENT: PyElement = PyElement(None);
}

pub struct PyElement(Option<Py<PyAny>>);

use pyo3::types::PyAny;

impl PyElement {
    pub fn new(val: Py<PyAny>) -> Self {
        PyElement(Some(val))
    }

    pub fn as_ref(&self) -> &Py<PyAny> {
        self.0.as_ref().unwrap()
    }

    pub fn value(&self) -> Py<PyAny> {
        Python::attach(|py| self.as_ref().clone_ref(py))
    }
}

impl Default for PyElement {
    fn default() -> Self {
        Python::attach(|py| PyElement(Some(py.None())))
    }
}

impl std::fmt::Debug for PyElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Python::attach(|py| {
            let res = self
                .as_ref()
                .call_method0(py, "__str__")
                .unwrap()
                .extract::<String>(py)
                .unwrap();
            write!(f, "{}", res)
        })
    }
}

impl Clone for PyElement {
    fn clone(&self) -> Self {
        match &self.0 {
            Some(inner) => Python::attach(|py| PyElement(Some(inner.clone_ref(py)))),
            None => PyElement(None),
        }
    }
}

impl PartialEq for PyElement {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => {
                Python::attach(|py| {
                    // keep cloned Py<T> alive outside bind()
                    let a_obj = a.clone_ref(py);
                    let b_obj = b.clone_ref(py);

                    // bind inside attach so temporary doesn't drop too early
                    let a_bound = a_obj.bind(py);
                    let b_bound = b_obj.bind(py);

                    match a_bound.rich_compare(&b_bound, pyo3::basic::CompareOp::Eq) {
                        Ok(obj) => obj.is_truthy().unwrap_or(false),
                        Err(_) => false,
                    }
                })
            }
            (None, None) => true,
            _ => false,
        }
    }
}

pub trait ToPyResult<T> {
    fn to_pyresult(self) -> PyResult<T>;
}

impl<T> ToPyResult<T> for anyhow::Result<T> {
    fn to_pyresult(self) -> PyResult<T> {
        self.map_err(|e| PyException::new_err(e.to_string()))
    }
}

pub fn parse_run_args(
    py: Python<'_>,
    realtime: Option<bool>,
    start: Option<Py<PyAny>>,
    duration: Option<Py<PyAny>>,
    cycles: Option<u32>,
) -> anyhow::Result<(RunMode, RunFor)> {
    if duration.is_some() && cycles.is_some() {
        panic!("Cannot specify both duration and cycles");
    }
    let realtime = realtime.unwrap_or(false);
    if realtime && start.is_some() {
        panic!("Cannot specify start in realtime mode");
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
