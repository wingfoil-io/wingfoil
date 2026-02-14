use derive_more::Display;
use lazy_static::lazy_static;
use pyo3::prelude::*;

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use ::wingfoil::{GraphState, IntoNode, MutableNode, StreamPeekRef, UpStreams};

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
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        Python::attach(|py| {
            let this = self.0.bind(py);
            let res = this
                .call_method0("cycle")
                .map_err(|e| anyhow::anyhow!("Failed to call cycle method: {e}"))?;
            res.extract::<bool>()
                .map_err(|e| anyhow::anyhow!("Failed to extract boolean result: {e}"))
        })
    }

    fn upstreams(&self) -> UpStreams {
        let ups = Python::attach(|py| {
            let this = self.0.bind(py);
            let res = this.call_method0("upstreams").unwrap();
            let res = res.extract::<Vec<Py<PyAny>>>().unwrap();
            res.iter()
                .filter_map(|obj| {
                    let bound = obj.bind(py);
                    if let Ok(stream) = bound.extract::<PyStream>() {
                        Some(stream.inner_stream().as_node())
                    } else if let Ok(stream) = bound.extract::<PyProxyStream>() {
                        Some(stream.into_node())
                    } else {
                        log::warn!("Unexpected upstream type: {:?}, skipping", bound.get_type());
                        None
                    }
                })
                .collect::<Vec<_>>()
        });
        UpStreams::new(ups, vec![])
    }
}

lazy_static! {
    pub static ref DUMMY_PY_ELEMENT: PyElement = PyElement::none();
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
