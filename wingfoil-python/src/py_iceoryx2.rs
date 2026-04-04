//! Python bindings for the iceoryx2 adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use std::rc::Rc;
use wingfoil::adapters::iceoryx2::{
    FixedBytes, Iceoryx2Mode, Iceoryx2ServiceVariant, Iceoryx2SubOpts, iceoryx2_pub_with,
    iceoryx2_sub_opts,
};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Maximum data size for zero-copy transfers between Python processes (64KB).
pub const ICEORYX2_MAX_DATA_SIZE: usize = 65536;
pub type PyFixedBytes = FixedBytes<ICEORYX2_MAX_DATA_SIZE>;

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
}

impl From<PyIceoryx2Mode> for Iceoryx2Mode {
    fn from(v: PyIceoryx2Mode) -> Self {
        match v {
            PyIceoryx2Mode::Spin => Iceoryx2Mode::Spin,
            PyIceoryx2Mode::Threaded => Iceoryx2Mode::Threaded,
        }
    }
}

/// Subscribe to an iceoryx2 service.
///
/// Args:
///     service_name: iceoryx2 service name, e.g. `"my/service"`
///     variant: Service variant ("ipc" or "local")
///     mode: Polling mode ("spin" or "threaded")
#[pyfunction]
#[pyo3(signature = (service_name, variant=PyIceoryx2ServiceVariant::Ipc, mode=PyIceoryx2Mode::Spin))]
pub fn py_iceoryx2_sub(
    service_name: String,
    variant: PyIceoryx2ServiceVariant,
    mode: PyIceoryx2Mode,
) -> PyStream {
    let opts = Iceoryx2SubOpts {
        variant: variant.into(),
        mode: mode.into(),
    };
    let stream = iceoryx2_sub_opts::<PyFixedBytes>(&service_name, opts);

    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|fb| PyBytes::new(py, fb.as_slice()).into_any().unbind())
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// Inner implementation for the `.iceoryx2_pub()` stream method.
pub fn py_iceoryx2_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    service_name: String,
    variant: PyIceoryx2ServiceVariant,
) -> Rc<dyn Node> {
    let burst_stream: Rc<dyn Stream<Burst<PyFixedBytes>>> = stream.map(move |elem| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(bytes) = obj.extract::<Vec<u8>>() {
                let mut b = Burst::new();
                b.push(PyFixedBytes::new(&bytes));
                b
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| {
                        item.extract::<Vec<u8>>()
                            .ok()
                            .map(|bytes| PyFixedBytes::new(&bytes))
                    })
                    .collect()
            } else {
                log::error!("iceoryx2_pub: stream value must be bytes or list of bytes");
                Burst::new()
            }
        })
    });

    iceoryx2_pub_with(burst_stream, &service_name, variant.into())
}
