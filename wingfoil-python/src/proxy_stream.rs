use derive_more::Display;

use ::wingfoil::{GraphState, IntoNode, MutableNode, StreamPeekRef, UpStreams};

use pyo3::prelude::*;

// /use crate::proxy_stream::*;
use crate::py_stream::*;
use crate::types::*;

/// This is used as inner class of python coded base class Stream
#[derive(Display)]
#[pyclass(subclass, unsendable)]
pub struct PyProxyStream(Py<PyAny>);

#[pymethods]
impl PyProxyStream {
    /// Constructor taking a Python object to wrap
    #[new]
    pub fn new(obj: Py<PyAny>) -> Self {
        PyProxyStream(obj)
    }
}

impl Clone for PyProxyStream {
    fn clone(&self) -> Self {
        Python::attach(|py| Self(self.0.clone_ref(py)))
    }
}

impl MutableNode for PyProxyStream {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        Python::attach(|py| {
            let this = self.0.bind(py);
            let res = this.call_method0("cycle").unwrap();
            res.extract::<bool>().unwrap()
        })
    }

    fn upstreams(&self) -> UpStreams {
        let ups = Python::attach(|py| {
            let this = self.0.bind(py);
            let res = this.call_method0("upstreams").unwrap();
            let res = res.extract::<Vec<Py<PyAny>>>().unwrap();
            res.iter()
                .map(|obj| {
                    let bound = obj.bind(py);
                    if let Ok(stream) = bound.extract::<PyStream>() {
                        stream.inner_stream().as_node()
                    } else if let Ok(stream) = bound.extract::<PyProxyStream>() {
                        stream.into_node()
                    } else {
                        panic!("Unexpected upstream type");
                    }
                })
                .collect::<Vec<_>>()
        });
        UpStreams::new(ups, vec![])
    }
}

impl StreamPeekRef<PyElement> for PyProxyStream {
    // This is a bit hacky - we supply dummy value for peek ref
    // but resolve it to real value in from_cell_ref.
    // Currently peek_ref is only used directly in demux.

    fn peek_ref(&self) -> &PyElement {
        &DUMMY_PY_ELEMENT
    }

    fn clone_from_cell_ref(&self, _cell_ref: std::cell::Ref<'_, PyElement>) -> PyElement {
        Python::attach(|py| {
            let res = self.0.call_method0(py, "peek").unwrap();
            PyElement::new(res)
        })
    }
}
