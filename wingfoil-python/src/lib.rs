mod proxy_stream;
mod py_element;
mod py_stream;
mod types;

use ::wingfoil::{Node, NodeOperators};
use py_element::*;
use py_stream::*;

use pyo3::prelude::*;
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
    /// Counts how many times upstream node has ticked.
    fn count(&self) -> PyStream {
        self.0.count().as_py_stream()
    }
}

/// A node that ticks at the specified period
#[pyfunction]
fn ticker(seconds: f64) -> PyNode {
    let ticker = ::wingfoil::ticker(Duration::from_secs_f64(seconds));
    PyNode::new(ticker)
}

/// A atream that ticks once, on first engine cycle
#[pyfunction]
fn constant(val: Py<PyAny>) -> PyStream {
    let strm = ::wingfoil::constant(PyElement::new(val));
    PyStream(strm)
}

/// maps steams a amd b into a new stream using func (e.g lambda a, b: a + b)
#[pyfunction]
fn bimap(a: Py<PyAny>, b: Py<PyAny>, func: Py<PyAny>) -> PyStream {
    Python::attach(|py| {
        let a = a
            .as_ref()
            .extract::<PyRef<PyStream>>(py)
            .unwrap()
            .inner_stream();
        let b = b
            .as_ref()
            .extract::<PyRef<PyStream>>(py)
            .unwrap()
            .inner_stream();
        let stream = ::wingfoil::bimap(a, b, move |a: PyElement, b: PyElement| {
            Python::attach(|py: Python<'_>| {
                let res = func.call1(py, (a.value(), b.value())).unwrap();
                PyElement::new(res)
            })
        });
        PyStream(stream)
    })
}

/// Wingfoil is a blazingly fast, highly scalable stream processing
/// framework designed for latency-critical use cases such as electronic
/// trading and real-time AI systems
#[pymodule]
fn _wingfoil(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();
    module.add_function(wrap_pyfunction!(ticker, module)?)?;
    module.add_function(wrap_pyfunction!(constant, module)?)?;
    module.add_function(wrap_pyfunction!(bimap, module)?)?;
    module.add_class::<PyNode>()?;
    module.add_class::<PyStream>()?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
