mod proxy_stream;
mod py_element;
mod py_kdb;
mod py_stream;
mod types;

// Removed unused 'Graph' import
use ::wingfoil::{Dep, Node, NodeOperators};
use py_element::*;
use py_stream::*;
use types::ToPyResult;

use pyo3::prelude::*;
use std::rc::Rc;
use std::time::Duration;

#[pyclass(unsendable, name = "Node")]
#[derive(Clone)]
pub(crate) struct PyNode(Rc<dyn Node>);

impl PyNode {
    pub(crate) fn new(node: Rc<dyn Node>) -> Self {
        Self(node)
    }
}

#[pymethods]
impl PyNode {
    fn count(&self) -> PyStream {
        self.0.count().as_py_stream()
    }

    #[pyo3(signature = (realtime=true, start=None, duration=None, cycles=None))]
    fn run(
        &self,
        py: Python<'_>,
        realtime: Option<bool>,
        start: Option<Py<PyAny>>,
        duration: Option<Py<PyAny>>,
        cycles: Option<u32>,
    ) -> PyResult<()> {
        let (run_mode, run_for) =
            types::parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;

        let node_ptr = Rc::as_ptr(&self.0);
        let (addr, vtable): (usize, usize) = unsafe { std::mem::transmute(node_ptr) };

        let result = py.detach(move || {
            let node_ptr: *const dyn Node = unsafe { std::mem::transmute((addr, vtable)) };
            let node = unsafe { Rc::from_raw(node_ptr) };
            let result = ::wingfoil::NodeOperators::run(&node, run_mode, run_for);
            std::mem::forget(node);
            result
        });
        result.to_pyresult()?;
        Ok(())
    }
}

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
        let stream = ::wingfoil::bimap(
            Dep::Active(a),
            Dep::Active(b),
            move |a: PyElement, b: PyElement| {
                Python::attach(|py: Python<'_>| {
                    let res = func.call1(py, (a.value(), b.value())).unwrap();
                    PyElement::new(res)
                })
            },
        );
        PyStream(stream)
    })
}

#[pyclass(unsendable, name = "Graph")]
#[derive(Clone)]
pub(crate) struct PyGraph(Vec<Rc<dyn Node>>);

#[pymethods]
impl PyGraph {
    #[new]
    fn new(nodes: Vec<Py<PyAny>>) -> PyResult<Self> {
        Python::attach(|py| {
            let mut roots: Vec<Rc<dyn Node>> = Vec::new();
            for obj in nodes {
                if let Ok(stream) = obj.extract::<PyRef<PyStream>>(py) {
                    roots.push(stream.0.clone().as_node());
                } else if let Ok(node) = obj.extract::<PyRef<PyNode>>(py) {
                    roots.push(node.0.clone());
                } else {
                    return Err(pyo3::exceptions::PyTypeError::new_err(
                        "Graph components must be Stream or Node",
                    ));
                }
            }
            Ok(PyGraph(roots))
        })
    }

    #[pyo3(signature = (realtime=true, start=None, duration=None, cycles=None))]
    fn run(
        &self,
        py: Python<'_>,
        realtime: Option<bool>,
        start: Option<Py<PyAny>>,
        duration: Option<Py<PyAny>>,
        cycles: Option<u32>,
    ) -> PyResult<()> {
        let (run_mode, run_for) =
            types::parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;

        let mut ptrs: Vec<(usize, usize)> = Vec::with_capacity(self.0.len());
        for node in &self.0 {
            let node_ptr = Rc::as_ptr(node);
            let (addr, vtable): (usize, usize) = unsafe { std::mem::transmute(node_ptr) };
            ptrs.push((addr, vtable));
        }

        let result = py.detach(move || {
            let mut roots: Vec<Rc<dyn Node>> = Vec::with_capacity(ptrs.len());
            for (addr, vtable) in ptrs {
                let node_ptr: *const dyn Node = unsafe { std::mem::transmute((addr, vtable)) };
                let node = unsafe { Rc::from_raw(node_ptr) };
                roots.push(node.clone());
                std::mem::forget(node);
            }

            let mut graph = ::wingfoil::Graph::new(roots, run_mode, run_for);
            graph.run()
        });
        result.to_pyresult()?;
        Ok(())
    }
}

#[pymodule]
fn _wingfoil(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();
    module.add_function(wrap_pyfunction!(ticker, module)?)?;
    module.add_function(wrap_pyfunction!(constant, module)?)?;
    module.add_function(wrap_pyfunction!(bimap, module)?)?;
    module.add_function(wrap_pyfunction!(py_kdb::py_kdb_read, module)?)?;
    module.add_function(wrap_pyfunction!(py_kdb::py_kdb_write, module)?)?;
    module.add_class::<PyNode>()?;
    module.add_class::<PyStream>()?;
    module.add_class::<PyGraph>()?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
