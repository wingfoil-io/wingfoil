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
pub struct PyStream(Rc<dyn Stream<PyElement>>);

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

    // // Helper: convert Vec<PyElement> -> Py<PyAny> list
    // fn py_list_from_vec(py: Python<'_>, v: Vec<PyElement>) -> Py<PyAny> {
    //     let list = PyList::empty(py);
    //     for e in v {
    //         if let Some(obj) = e.0 {
    //             list.append(obj.as_ref(py)).expect("append failed");
    //         } else {
    //             list.append(py.None()).expect("append none failed");
    //         }
    //     }
    //     list.into()
    // }

    // begin StreamOperators

    // /// accumulate the source into a Python list (PyElement containing list)
    // fn accumulate(&self) -> PyStream {
    //     let s = self.0.accumulate(); // Stream<Vec<PyElement>>
    //     let s = s.map(move |vec: Vec<PyElement>| {
    //         Python::attach(|py| PyElement(Some(Self::py_list_from_vec(py, vec))))
    //     });
    //     PyStream(s)
    // }

    fn average(&self) -> PyStream {
        self.extract::<f64>().average().as_py_stream()
    }

    // /// Buffer the source stream. The buffer is flushed automatically.
    // fn buffer(&self, capacity: usize) -> PyStream {
    //     let s = self.0.buffer(capacity); // Stream<Vec<PyElement>>
    //     let s = s.map(move |vec: Vec<PyElement>| {
    //         Python::attach(|py| PyElement(Some(Self::py_list_from_vec(py, vec))))
    //     });
    //     PyStream(s)
    // }

    // /// Used to accumulate values retrievable after graph completes. Returns list of (value, time) tuples.
    // fn collect(&self) -> PyStream {
    //     let s = self.0.collect(); // Stream<Vec<ValueAt<PyElement>>>
    //     let s = s.map(move |vec_va: Vec<ValueAt<PyElement>>| {
    //         Python::attach(|py| {
    //             let list = PyList::empty(py);
    //             for va in vec_va {
    //                 // convert value to Python object (or None)
    //                 let val_obj = match va.value.0 {
    //                     Some(obj) => obj.as_ref(py).into(),
    //                     None => py.None().into(),
    //                 };
    //                 // Represent ValueAt as tuple (value, time)
    //                 let tup = (val_obj, va.time);
    //                 list.append(tup).unwrap();
    //             }
    //             PyElement(Some(list.into()))
    //         })
    //     });
    //     PyStream(s)
    // }

    // /// collapses a burst (IntoIterator) of ticks into a single tick
    // fn collapse(&self) -> PyStream {
    //     // Underlying: collapse::<OUT>() where OUT is PyElement or Py<PyAny>
    //     // We assume collapse returns Stream<PyElement> or Stream<Py<PyAny>>; adapt if needed.
    //     let s = self.0.collapse::<Py<PyAny>>();
    //     let s = s.map(move |out: Py<PyAny>| PyElement(Some(out)));
    //     PyStream(s)
    // }

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

    // /// reduce/fold source by applying Python callable to accumulator
    // fn fold(&self, func: Py<PyAny>) -> PyStream {
    //     // We'll use a PyElement accumulator; Python callable signature: (acc, value) -> new_acc (optional)
    //     let s = self.0.fold(move |acc: &mut PyElement, pe: PyElement| {
    //         Python::attach(|py| {
    //             let f = func.as_ref(py);
    //             let acc_obj = match acc.0 {
    //                 Some(ref o) => o.as_ref(py),
    //                 None => py.None(),
    //             };
    //             let val_obj = match pe.0 {
    //                 Some(ref o) => o.as_ref(py),
    //                 None => py.None(),
    //             };
    //             if let Ok(ret) = f.call1((acc_obj, val_obj)) {
    //                 // replace accumulator with returned value if callable returned something
    //                 *acc = PyElement(Some(ret.into()));
    //             }
    //         })
    //     });
    //     PyStream(s)
    // }

    // /// difference in its source from one cycle to the next (pass-through of PyElement)
    // fn difference(&self) -> PyStream {
    //     let s = self.0.difference();
    //     let s = s.map(|pe: PyElement| pe);
    //     PyStream(s)
    // }

    // /// Propagates its source delayed by specified duration (milliseconds)
    // fn delay(&self, delay_ms: u64) -> PyStream {
    //     let d = Duration::from_millis(delay_ms);
    //     let s = self.0.delay(d);
    //     let s = s.map(|pe: PyElement| pe);
    //     PyStream(s)
    // }

    /// only propagates its source if it changed (uses PartialEq on PyElement)
    fn distinct(&self) -> PyStream {
        PyStream(self.0.distinct())
    }

    // /// drops source contingent on supplied stream (expect the user to pass a PyStream)
    // fn filter(&self, condition: &PyAny) -> PyResult<PyStream> {
    //     // Best-effort: accept a PyStream instance and extract its inner Rc<dyn Stream<bool>>
    //     Python::with_gil(|py| {
    //         // Try to downcast to PyCell<PyStream> and borrow .0; this requires exposing PyStream as a Python class
    //         if let Ok(pystream_cell) = condition.downcast::<pyo3::PyCell<PyStream>>() {
    //             let other = pystream_cell.borrow();
    //             // Here we assume other.0 is Rc<dyn Stream<PyElement>> but it should be Stream<bool> for condition.
    //             // The user must pass a PyStream that yields bool PyElements.
    //             let cond_stream_rc = other.0.clone();
    //             // wrap the cond_stream_rc into an Rc<dyn Stream<bool>> via a mapping that converts PyElement -> bool
    //             // For simplicity build a condition stream using `.map` on cond_stream_rc
    //             let cond_mapped = cond_stream_rc.map(|pe: PyElement| {
    //                 Python::attach(|py| {
    //                     match pe.0 {
    //                         Some(obj) => obj.as_ref(py).extract::<bool>().unwrap_or(false),
    //                         None => false,
    //                     }
    //                 })
    //             });
    //             let filtered = self.0.filter(Rc::new(cond_mapped));
    //             let mapped = filtered.map(|pe: PyElement| pe);
    //             Ok(PyStream(mapped))
    //         } else {
    //             Err(pyo3::exceptions::PyTypeError::new_err(
    //                 "filter expects a PyStream instance yielding bools",
    //             ))
    //         }
    //     })
    // }

    // /// drops source contingent on supplied predicate (Python callable)
    // fn filter_value(&self, predicate: Py<PyAny>) -> PyStream {
    //     let s = self.0.filter_value(move |pe: &PyElement| {
    //         Python::attach(|py| {
    //             let f = predicate.as_ref(py);
    //             let arg = match pe.0 {
    //                 Some(ref o) => o.as_ref(py),
    //                 None => py.None(),
    //             };
    //             f.call1((arg,)).unwrap().extract::<bool>().unwrap_or(false)
    //         })
    //     });
    //     let s = s.map(|pe: PyElement| pe);
    //     PyStream(s)
    // }

    /// propagates source up to limit times
    fn limit(&self, limit: u32) -> PyStream {
        PyStream(self.0.limit(limit))
    }

    /// logs source and propagates it. Default level INFO.
    fn logged(&self, label: String) -> PyStream {
        PyStream(self.0.logged(&label, Level::Info))
    }

    /// Map’s its source into a new Stream using the supplied Python callable.
    fn map(&self, func: Py<PyAny>) -> PyResult<PyStream> {
        let stream = self.0.map(move |x| {
            Python::attach(|py| {
                let res = func.call1(py, (x.value(),)).unwrap();
                PyElement::new(res)
            })
        });
        Ok(PyStream(stream))
    }

    // /// Uses func to build graph, which is spawned on worker thread.
    // /// This is tricky to bridge to Python; leave as NotImplementedError with guidance.
    // fn mapper(&self, _func: Py<PyAny>) -> PyResult<PyStream> {
    //     Err(pyo3::exceptions::PyNotImplementedError::new_err(
    //         "mapper: please implement a Rust-side wrapper or provide a concrete Python->Rust graph builder bridge",
    //     ))
    // }

    // /// negates its input (for boolean-like PyElements)
    fn not(&self) -> PyStream {
        PyStream(self.0.not())
    }

    // /// reduce by applying python callable pairwise (func(a, b) -> out)
    // fn reduce(&self, func: Py<PyAny>) -> PyStream {
    //     let s = self.0.reduce(move |a: PyElement, b: PyElement| {
    //         Python::attach(|py| {
    //             let f = func.as_ref(py);
    //             let av = match a.0 { Some(ref o) => o.as_ref(py), None => py.None() };
    //             let bv = match b.0 { Some(ref o) => o.as_ref(py), None => py.None() };
    //             let out = f.call1((av, bv)).unwrap();
    //             PyElement(Some(out.into()))
    //         })
    //     });
    //     PyStream(s)
    // }

    // /// samples its source on each tick of trigger (trigger must be a PyStream)
    // fn sample(&self, trigger: &PyAny) -> PyResult<PyStream> {
    //     Python::with_gil(|py| {
    //         if let Ok(pystream_cell) = trigger.downcast::<pyo3::PyCell<PyStream>>() {
    //             let other = pystream_cell.borrow();
    //             // assume other.0 is a Node/Stream we can provide to sample
    //             // For safety, return NotImplemented unless you have an exposed Node type bridging
    //             Err(pyo3::exceptions::PyNotImplementedError::new_err(
    //                 "sample: implement by extracting trigger's node and calling inner.sample(trigger_node).",
    //             ))
    //         } else {
    //             Err(pyo3::exceptions::PyTypeError::new_err(
    //                 "sample expects a PyStream trigger argument",
    //             ))
    //         }
    //     })
    // }

    // /// print stream values to stdout via Python print()
    // fn print(&self) -> PyStream {
    //     let s = self.0.for_each(move |pe: PyElement, _nano: i128| {
    //         Python::attach(|py| {
    //             if let Some(obj) = pe.0 {
    //                 let _ = py.print(obj.as_ref(py));
    //             } else {
    //                 let _ = py.print(py.None());
    //             }
    //         })
    //     });
    //     // for_each returns Node; if your for_each combinator returns Stream, adapt accordingly
    //     PyStream(s)
    // }

    // /// sum the stream (assumes addable PyElements)
    // fn sum(&self) -> PyStream {
    //     let s = self.0.sum();
    //     let s = s.map(|pe: PyElement| pe);
    //     PyStream(s)
    // }

    // end StreamOperators
}
