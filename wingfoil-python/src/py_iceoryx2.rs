//! Python bindings for the iceoryx2 adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::exceptions::PyTypeError;
use pyo3::types::{PyBytes, PyList};
use std::rc::Rc;
use wingfoil::adapters::iceoryx2::{
    Iceoryx2Mode, Iceoryx2PubSliceOpts, Iceoryx2ServiceVariant, Iceoryx2SubOpts,
    iceoryx2_pub_slice_opts, iceoryx2_sub_slice_opts,
};
use wingfoil::{Node, Stream, StreamOperators};

#[pyclass(eq, eq_int, name = "Iceoryx2ServiceVariant")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyIceoryx2ServiceVariant {
    Ipc,
    Local,
}

impl From<PyIceoryx2ServiceVariant> for Iceoryx2ServiceVariant {
    fn from(v: PyIceoryx2ServiceVariant) -> Self {
        match v {
            PyIceoryx2ServiceVariant::Ipc => Iceoryx2ServiceVariant::Ipc,
            PyIceoryx2ServiceVariant::Local => Iceoryx2ServiceVariant::Local,
        }
    }
}

#[pyclass(eq, eq_int, name = "Iceoryx2Mode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyIceoryx2Mode {
    Spin,
    Threaded,
    Signaled,
}

impl From<PyIceoryx2Mode> for Iceoryx2Mode {
    fn from(v: PyIceoryx2Mode) -> Self {
        match v {
            PyIceoryx2Mode::Spin => Iceoryx2Mode::Spin,
            PyIceoryx2Mode::Threaded => Iceoryx2Mode::Threaded,
            PyIceoryx2Mode::Signaled => Iceoryx2Mode::Signaled,
        }
    }
}

/// Subscribe to an iceoryx2 service.
///
/// Args:
///     service_name: iceoryx2 service name, e.g. `"my/service"`
///     variant: Service variant ("ipc" or "local")
///     mode: Polling mode ("spin", "threaded", or "signaled")
///     history_size: Subscriber history ring size (late-joiner buffer)
#[pyfunction]
#[pyo3(signature = (service_name, variant=PyIceoryx2ServiceVariant::Ipc, mode=PyIceoryx2Mode::Spin, history_size=5))]
pub fn py_iceoryx2_sub(
    service_name: String,
    variant: PyIceoryx2ServiceVariant,
    mode: PyIceoryx2Mode,
    history_size: usize,
) -> PyStream {
    let opts = Iceoryx2SubOpts {
        variant: variant.into(),
        mode: mode.into(),
        history_size,
    };

    // Use the slice-based subscriber for generic bytes
    let stream = iceoryx2_sub_slice_opts(&service_name, opts);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|bytes| PyBytes::new(py, &bytes).into_any().unbind())
                .collect();
            let list = PyList::new(py, items).unwrap_or_else(|e| {
                log::error!("iceoryx2_sub: failed to allocate list: {e}");
                PyList::empty(py)
            });
            PyElement::new(list.into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.iceoryx2_pub()` stream method.
pub fn py_iceoryx2_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    service_name: String,
    variant: PyIceoryx2ServiceVariant,
    history_size: usize,
    initial_max_slice_len: usize,
) -> Rc<dyn Node> {
    let burst_stream = stream.try_map(move |elem| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(bytes) = obj.extract::<Vec<u8>>() {
                let mut b = wingfoil::Burst::new();
                b.push(bytes);
                Ok(b)
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                let mut b = wingfoil::Burst::new();
                for item in list.iter() {
                    let bytes = item.extract::<Vec<u8>>().map_err(|_| {
                        anyhow::anyhow!(PyTypeError::new_err(
                            "iceoryx2_pub: list items must be bytes",
                        ))
                    })?;
                    b.push(bytes);
                }
                Ok(b)
            } else {
                Err(anyhow::anyhow!(PyTypeError::new_err(
                    "iceoryx2_pub: stream value must be bytes or list[bytes]",
                )))
            }
        })
    });

    let opts = Iceoryx2PubSliceOpts {
        variant: variant.into(),
        history_size,
        initial_max_slice_len,
    };
    iceoryx2_pub_slice_opts(burst_stream, &service_name, opts)
}
