use log::Level;
use pyo3::BoundObject;
use std::any::type_name;

use ::wingfoil::{Element, IntoStream, NodeOperators, Stream, StreamOperators};

use pyo3::conversion::IntoPyObject;
use pyo3::prelude::*;

use std::rc::Rc;

use crate::proxy_stream::*;
use crate::py_element::PyElement;
use crate::types::*;
use crate::*;

#[derive(Clone)]
#[pyclass(subclass, unsendable)]
pub struct PyStream(pub Rc<dyn Stream<PyElement>>);

impl PyStream {
    fn extract<T>(&self) -> Rc<dyn Stream<T>>
    where
        T: Element + for<'a, 'py> FromPyObject<'a, 'py>,
    {
        self.0.map(move |x: PyElement| {
            Python::attach(|py| match x.as_ref().extract::<T>(py) {
                Ok(val) => val,
                Err(_err) => {
                    panic!("Failed to convert from python type to native rust type")
                }
            })
        })
    }

    pub fn inner_stream(&self) -> Rc<dyn Stream<PyElement>> {
        self.0.clone()
    }

    pub fn from_inner(inner: Rc<dyn Stream<PyElement>>) -> Self {
        Self(inner)
    }
}

pub fn to_pyany<T>(x: T) -> Py<PyAny>
where
    T: for<'py> IntoPyObject<'py>,
{
    Python::attach(|py| match x.into_pyobject(py) {
        Ok(bound) => bound.into_any().unbind(),
        Err(_) => panic!("Conversion to PyAny from type {} failed", type_name::<T>()),
    })
}

pub fn vec_any_to_pyany(x: Vec<Py<PyAny>>) -> Py<PyAny> {
    Python::attach(|py| x.into_pyobject(py).unwrap().into_any().unbind())
}

pub trait AsPyStream<T>
where
    T: Element + for<'py> IntoPyObject<'py>,
{
    fn as_py_stream(&self) -> PyStream;
}

impl<T> AsPyStream<T> for Rc<dyn Stream<T>>
where
    T: Element + for<'py> IntoPyObject<'py>,
{
    fn as_py_stream(&self) -> PyStream {
        let strm = self.map(|x| {
            let py_any = to_pyany(x);
            PyElement::new(py_any)
        });
        PyStream(strm)
    }
}

#[pymethods]
impl PyStream {
    #[new]
    fn new(inner: Py<PyAny>) -> Self {
        let stream = PyProxyStream::new(inner);
        let stream = stream.into_stream();
        Self(stream)
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
            parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;
        self.0.run(run_mode, run_for).to_pyresult()?;
        Ok(())
    }

    fn peek_value(&self) -> Py<PyAny> {
        self.0.peek_value().value()
    }

    // begin StreamOperators

    // not done yet:
    //   mapper
    //   reduce
    //   print
    //   collect
    //   collapse
    //   fold
    // will not be done?
    //   mapper
    //   accumulate
    //   filter (as opposed to filter_value)

    fn average(&self) -> PyStream {
        self.extract::<f64>().average().as_py_stream()
    }

    fn buffer(&self, capacity: usize) -> PyStream {
        let strm = self.0.buffer(capacity).map(|items| {
            Python::attach(move |py| {
                let items = items
                    .iter()
                    .map(|item| item.as_ref().clone_ref(py))
                    .collect::<Vec<_>>();
                PyElement::new(vec_any_to_pyany(items))
            })
        });
        PyStream(strm)
    }

    fn finally(&self, func: Py<PyAny>) -> PyNode {
        let node = self.0.finally(|py_elmnt, _| {
            Python::attach(move |py| {
                let res = py_elmnt.as_ref().clone_ref(py);
                let args = (res,);
                func.call1(py, args).unwrap();
            });
        });
        PyNode(node)
    }

    fn for_each(&self, func: Py<PyAny>) -> PyNode {
        let node = self.0.for_each(move |py_elmnt, t| {
            Python::attach(|py| {
                let res = py_elmnt.as_ref().clone_ref(py);
                let t: f64 = t.into();
                let args = (res, t);
                func.call1(py, args).unwrap();
            });
        });
        PyNode(node)
    }

    /// difference in its source from one cycle to the next (pass-through of PyElement)
    fn difference(&self) -> PyStream {
        PyStream(self.0.difference())
    }

    /// Propagates its source delayed by specified duration (milliseconds)
    fn delay(&self, delay_secs: f64) -> PyStream {
        let delay = Duration::from_secs_f64(delay_secs);
        PyStream(self.0.delay(delay))
    }

    /// only propagates its source if it changed (uses PartialEq on PyElement)
    fn distinct(&self) -> PyStream {
        PyStream(self.0.distinct())
    }

    /// drops source contingent on supplied predicate (Python callable)
    fn filter(&self, keep_func: Py<PyAny>) -> PyStream {
        let keep = self.0.map(move |x| {
            Python::attach(|py| {
                keep_func
                    .call1(py, (x.value(),))
                    .unwrap()
                    .extract::<bool>(py)
                    .unwrap()
            })
        });
        PyStream(self.0.filter(keep))
    }

    /// propagates source up to limit times
    fn limit(&self, limit: u32) -> PyStream {
        PyStream(self.0.limit(limit))
    }

    /// logs source and propagates it. Default level INFO.
    fn logged(&self, label: String) -> PyStream {
        PyStream(self.0.logged(&label, Level::Info))
    }

    /// Map’s its source into a new Stream using the supplied Python callable.
    fn map(&self, func: Py<PyAny>) -> PyStream {
        let stream = self.0.map(move |x| {
            Python::attach(|py| {
                let res = func.call1(py, (x.value(),)).unwrap();
                PyElement::new(res)
            })
        });
        PyStream(stream)
    }

    // /// negates its input (for boolean-like PyElements)
    fn not(&self) -> PyStream {
        PyStream(self.0.not())
    }

    fn sample(&self, trigger: Py<PyAny>) -> PyStream {
        Python::attach(|py| {
            let obj = trigger.as_ref();
            if let Ok(node) = obj.extract::<PyRef<PyNode>>(py) {
                return PyStream(self.0.sample(node.0.clone()));
            }
            if let Ok(stream) = obj.extract::<PyRef<PyStream>>(py) {
                return PyStream(self.0.sample(stream.0.clone()));
            }
            panic!("Expected a PyNode or PyStream");
        })
    }

    /// sum the stream (assumes addable PyElements)
    fn sum(&self) -> PyStream {
        PyStream(self.0.sum())
    }

    // end StreamOperators
}
