//! Python bindings for ZMQ pub/sub adapters.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use std::rc::Rc;
use wingfoil::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub};
use wingfoil::{Node, Stream, StreamOperators};

/// Subscribe to a ZMQ PUB socket.
///
/// Returns a `(data_stream, status_stream)` tuple.
///
/// Each tick of `data_stream` yields a `list[bytes]` containing all messages
/// received in that cycle. Each tick of `status_stream` yields the string
/// `"connected"` or `"disconnected"`.
///
/// Args:
///     address: ZMQ endpoint to connect to (e.g. "tcp://localhost:5556")
///
/// Returns:
///     Tuple of (data_stream, status_stream)
#[pyfunction]
pub fn py_zmq_sub(address: String) -> (PyStream, PyStream) {
    let (data, status) =
        zmq_sub::<Vec<u8>>(address.as_str()).expect("direct address zmq_sub should not fail");
    streams_to_py(data, status)
}

/// Subscribe to a named ZMQ publisher via etcd service discovery.
///
/// Looks up `name` in etcd and connects to the publisher at the stored address.
/// The lookup happens at call time — ensure the publisher is registered before
/// calling this function.
///
/// Args:
///     name: Publisher name / etcd key (e.g. "quotes")
///     endpoint: etcd endpoint (e.g. "http://localhost:2379")
///
/// Returns:
///     Tuple of (data_stream, status_stream)
///
/// Raises:
///     RuntimeError: if the key is absent or etcd is unreachable
#[cfg(feature = "etcd")]
#[pyfunction]
pub fn py_zmq_sub_etcd(name: String, endpoint: String) -> PyResult<(PyStream, PyStream)> {
    use wingfoil::adapters::zmq::EtcdRegistry;
    let (data, status) = zmq_sub::<Vec<u8>>((name.as_str(), EtcdRegistry::new(endpoint)))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    Ok(streams_to_py(data, status))
}

fn streams_to_py(
    data: Rc<dyn wingfoil::Stream<wingfoil::Burst<Vec<u8>>>>,
    status: Rc<dyn wingfoil::Stream<ZmqStatus>>,
) -> (PyStream, PyStream) {
    let data_py = data.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|b| pyo3::types::PyBytes::new(py, &b).into_any().unbind())
                .collect();
            PyElement::new(
                pyo3::types::PyList::new(py, items)
                    .unwrap()
                    .into_any()
                    .unbind(),
            )
        })
    });

    let status_py = status.map(|s| {
        Python::attach(|py| {
            let s_str = match s {
                ZmqStatus::Connected => "connected",
                ZmqStatus::Disconnected => "disconnected",
            };
            PyElement::new(s_str.into_pyobject(py).unwrap().into_any().unbind())
        })
    });

    (PyStream(data_py), PyStream(status_py))
}

/// Inner implementation for `zmq_pub` that operates on the Rust stream level.
///
/// The stream must yield `bytes` values; they are extracted and published as-is.
pub fn py_zmq_pub_inner(stream: &Rc<dyn Stream<PyElement>>, port: u16) -> Rc<dyn Node> {
    let bytes_stream: Rc<dyn Stream<Vec<u8>>> = stream.map(|elem| {
        Python::attach(|py| {
            elem.as_ref().extract::<Vec<u8>>(py).unwrap_or_else(|e| {
                log::error!("zmq_pub: stream value is not bytes: {e}");
                Vec::new()
            })
        })
    });
    bytes_stream.zmq_pub(port, ())
}

/// Inner implementation for `zmq_pub_etcd` (etcd-based registration).
#[cfg(feature = "etcd")]
pub fn py_zmq_pub_etcd_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    name: String,
    port: u16,
    endpoint: String,
) -> Rc<dyn Node> {
    use wingfoil::adapters::zmq::EtcdRegistry;
    let registry = EtcdRegistry::new(endpoint);
    let bytes_stream = py_bytes_stream(stream, "zmq_pub_etcd");
    bytes_stream.zmq_pub(port, (name.as_str(), registry))
}

/// Inner implementation for `zmq_pub_etcd_on` (etcd-based, routable address).
#[cfg(feature = "etcd")]
pub fn py_zmq_pub_etcd_on_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    name: String,
    address: String,
    port: u16,
    endpoint: String,
) -> Rc<dyn Node> {
    use wingfoil::adapters::zmq::EtcdRegistry;
    let registry = EtcdRegistry::new(endpoint);
    let bytes_stream = py_bytes_stream(stream, "zmq_pub_etcd_on");
    bytes_stream.zmq_pub_on(&address, port, (name.as_str(), registry))
}

#[cfg(feature = "etcd")]
fn py_bytes_stream(
    stream: &Rc<dyn Stream<PyElement>>,
    label: &'static str,
) -> Rc<dyn Stream<Vec<u8>>> {
    stream.map(move |elem| {
        Python::attach(|py| {
            elem.as_ref().extract::<Vec<u8>>(py).unwrap_or_else(|e| {
                log::error!("{label}: stream value is not bytes: {e}");
                Vec::new()
            })
        })
    })
}
