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
    let (data, status) = zmq_sub::<Vec<u8>>(&address);

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
            elem.as_ref()
                .extract::<Vec<u8>>(py)
                .expect("zmq_pub requires stream values to be bytes")
        })
    });
    bytes_stream.zmq_pub(port)
}
