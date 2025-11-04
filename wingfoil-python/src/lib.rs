//mod time;
//use time::*;

use ::wingfoil::{Node, NodeOperators, RunFor, RunMode, Stream, StreamOperators};

use pyo3::conversion::IntoPyObjectExt;
use pyo3::prelude::*;

//use serde::Serialize;
use std::rc::Rc;
use std::time::Duration;

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
    fn run(&self) {
        self.0
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))
            .unwrap();
    }
    fn peek_value(&self) -> Py<PyAny> {
        self.0.peek_value().0
    }
    fn logged(&self, label: String) -> PyStream {
        PyStream(self.0.logged(&label, log::Level::Info))
    }
}

#[pymodule]
fn wingfoil_internal(module: &Bound<'_, PyModule>) -> PyResult<()> {
    _ = env_logger::try_init();
    module.add_function(wrap_pyfunction!(ticker, module)?)?;
    module.add_class::<PyNode>()?;
    Ok(())
}
