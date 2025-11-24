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

#[pyfunction]
fn constant(val: Py<PyAny>) -> PyStream {
    let strm = ::wingfoil::constant(PyElement::new(val));
    PyStream(strm)
}


/// Wingfoil is a blazingly fast, highly scalable stream processing 
/// framework designed for latency-critical use cases such as electronic 
/// trading and real-time AI systems
#[pymodule]
fn _wingfoil(module: &Bound<'_, PyModule>) -> PyResult<()> {
    _ = env_logger::try_init();
    module.add_function(wrap_pyfunction!(ticker, module)?)?;
    module.add_function(wrap_pyfunction!(constant, module)?)?;
    module.add_class::<PyNode>()?;
    module.add_class::<PyStream>()?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
