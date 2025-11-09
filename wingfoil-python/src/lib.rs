use anyhow::anyhow;
use pyo3::exceptions::PyException;
use std::time::SystemTime;

use ::wingfoil::{Node, NodeOperators, RunFor, RunMode, Stream, StreamOperators, NanoTime};

use pyo3::conversion::IntoPyObjectExt;
use pyo3::prelude::*;

use std::rc::Rc;
use std::time::Duration;
use std::time::UNIX_EPOCH;

#[pyclass(unsendable, name = "Node")]
#[derive(Clone)]
struct PyNode(Rc<dyn Node>);

impl PyNode {
    fn new(node: Rc<dyn Node>) -> Self {
        Self(node)
    }
}

#[pymethods]
impl PyNode {
    fn count(&self) -> PyResult<PyStream> {
        let count = self.0.count().map(move |x| {
            Python::attach(|py| {
                let x: Py<PyAny> = x.into_py_any(py).unwrap();
                PyElement(x)
            })
        });
        Ok(PyStream(count))
    }
}

#[pyfunction]
fn ticker(seconds: f64) -> PyResult<PyNode> {
    let ticker = ::wingfoil::ticker(Duration::from_secs_f64(seconds));
    let node = PyNode::new(ticker);
    Ok(node)
}

struct PyElement(Py<PyAny>);

impl Default for PyElement {
    fn default() -> Self {
        Python::attach(|py| PyElement(py.None()))
    }
}

impl std::fmt::Debug for PyElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Python::attach(|py| {
            let result = self.0.call_method0(py, "__str__").unwrap();
            write!(f, "{}", result.extract::<String>(py).unwrap())
        })
    }
}

impl Clone for PyElement {
    fn clone(&self) -> Self {
        Python::attach(|py| PyElement(self.0.clone_ref(py)))
    }
}

#[derive(Clone)]
#[pyclass(subclass, unsendable)]
struct PyStream(Rc<dyn Stream<PyElement>>);

#[pymethods]
impl PyStream {
    fn run(
        &self,
        py: Python<'_>,
        realtime: Option<bool>,
        start: Option<Py<PyAny>>,
        duration: Option<Py<PyAny>>,
        cycles: Option<u32>,
    ) -> PyResult<()> {
        let (run_mode, run_for) =
            parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;
        self.0.run(run_mode, run_for).to_pyresult()?;
        Ok(())
    }
    fn peek_value(&self) -> Py<PyAny> {
        self.0.peek_value().0
    }
    fn logged(&self, label: String) -> PyStream {
        PyStream(self.0.logged(&label, log::Level::Info))
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

fn parse_run_args(
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

#[pymodule]
fn _wingfoil(module: &Bound<'_, PyModule>) -> PyResult<()> {
    _ = env_logger::try_init();
    module.add_function(wrap_pyfunction!(ticker, module)?)?;
    module.add_class::<PyNode>()?;
    Ok(())
}
