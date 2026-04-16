//! Python bindings for the iceoryx2 adapter.

use crate::py_element::PyElement;
use crate::py_latency::{PyLatency, PyTracedBytes};
use crate::py_stream::PyStream;

use pyo3::prelude::*;
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
/// When `stages` is provided, incoming bytes are split into a latency header
/// (`[u64; len(stages)]` little-endian) followed by payload bytes. Each tick
/// yields a `list[TracedBytes]` instead of `list[bytes]`.
///
/// Args:
///     service_name: iceoryx2 service name, e.g. `"my/service"`
///     variant: Service variant ("ipc" or "local")
///     mode: Polling mode ("spin", "threaded", or "signaled")
///     history_size: Subscriber history ring size (late-joiner buffer)
///     stages: Optional list of latency stage names for instrumented mode
#[pyfunction]
#[pyo3(signature = (service_name, variant=PyIceoryx2ServiceVariant::Ipc, mode=PyIceoryx2Mode::Spin, history_size=5, stages=None))]
pub fn py_iceoryx2_sub(
    service_name: String,
    variant: PyIceoryx2ServiceVariant,
    mode: PyIceoryx2Mode,
    history_size: usize,
    stages: Option<Vec<String>>,
) -> PyStream {
    let opts = Iceoryx2SubOpts {
        variant: variant.into(),
        mode: mode.into(),
        history_size,
    };

    let stream = iceoryx2_sub_slice_opts(&service_name, opts);

    match stages {
        None => {
            // Original path: yield list[bytes]
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
        Some(stage_names) => {
            // Instrumented path: split [u64; N] header + payload → TracedBytes
            let header_len = stage_names.len() * 8;
            let py_stream = stream.map(move |burst| {
                let stage_names = stage_names.clone();
                Python::attach(|py| {
                    let items: Vec<Py<PyAny>> = burst
                        .into_iter()
                        .map(|bytes| {
                            if bytes.len() < header_len {
                                log::warn!(
                                    "iceoryx2_sub(stages): message too short ({} < {header_len}), skipping latency",
                                    bytes.len()
                                );
                                let payload = PyBytes::new(py, &bytes).into_any().unbind();
                                let lat = Py::new(py, PyLatency::create(stage_names.clone())).unwrap();
                                let traced = Py::new(py, PyTracedBytes::create(payload, lat)).unwrap();
                                return traced.into_any();
                            }
                            let lat = PyLatency::create_from_bytes(&bytes[..header_len], stage_names.clone());
                            let payload = PyBytes::new(py, &bytes[header_len..]).into_any().unbind();
                            let lat = Py::new(py, lat).unwrap();
                            let traced = Py::new(py, PyTracedBytes::create(payload, lat)).unwrap();
                            traced.into_any()
                        })
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
    }
}

/// Inner implementation for the `.iceoryx2_pub()` stream method.
///
/// When `stages` is provided, expects upstream to carry `TracedBytes` values.
/// Serializes as `latency.to_bytes() + payload` on the wire.
pub fn py_iceoryx2_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    service_name: String,
    variant: PyIceoryx2ServiceVariant,
    history_size: usize,
    initial_max_slice_len: usize,
    stages: Option<Vec<String>>,
) -> Rc<dyn Node> {
    let burst_stream = match stages {
        None => {
            // Original path: expects bytes or list[bytes]
            stream.try_map(move |elem| {
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
                                crate::types::py_type_error(
                                    "iceoryx2_pub: list items must be bytes",
                                )
                            })?;
                            b.push(bytes);
                        }
                        Ok(b)
                    } else {
                        Err(crate::types::py_type_error(
                            "iceoryx2_pub: stream value must be bytes or list[bytes]",
                        ))
                    }
                })
            })
        }
        Some(_stage_names) => {
            // Instrumented path: expects TracedBytes, packs header + payload
            stream.try_map(move |elem| {
                Python::attach(|py| {
                    let obj = elem.as_ref().bind(py);
                    let traced: PyRef<PyTracedBytes> = obj.extract().map_err(|_| {
                        crate::types::py_type_error(
                            "iceoryx2_pub(stages): expected TracedBytes value",
                        )
                    })?;
                    let lat = traced.latency.borrow(py);
                    let payload_bytes: Vec<u8> = traced.payload.extract(py).map_err(|_| {
                        crate::types::py_type_error(
                            "iceoryx2_pub(stages): TracedBytes.payload must be bytes",
                        )
                    })?;
                    // Pack: [u64; N] header (little-endian) ++ payload bytes
                    let header: Vec<u8> = lat
                        .stamps_ref()
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect();
                    let mut wire = Vec::with_capacity(header.len() + payload_bytes.len());
                    wire.extend_from_slice(&header);
                    wire.extend_from_slice(&payload_bytes);

                    let mut b = wingfoil::Burst::new();
                    b.push(wire);
                    Ok(b)
                })
            })
        }
    };

    let opts = Iceoryx2PubSliceOpts {
        variant: variant.into(),
        history_size,
        initial_max_slice_len,
    };
    iceoryx2_pub_slice_opts(burst_stream, &service_name, opts)
}
