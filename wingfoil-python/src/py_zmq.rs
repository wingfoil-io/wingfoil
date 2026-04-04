//! Python bindings for ZMQ pub/sub adapters.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use std::rc::Rc;
use wingfoil::adapters::zmq::{
    SeedHandle, SeedRegistry, ZeroMqPub, ZmqStatus, start_seed, zmq_sub,
};
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

/// Discover a named ZMQ publisher via seeds and subscribe to it.
///
/// Queries each seed in order until one resolves `name` to an address, then
/// connects. Returns an error if no seed can resolve the name.
///
/// Args:
///     name: Publisher name to look up (e.g. "quotes")
///     seeds: List of seed endpoints (e.g. ["tcp://localhost:7777"])
///
/// Returns:
///     Tuple of (data_stream, status_stream)
///
/// Raises:
///     RuntimeError: if no seed can resolve the name
#[pyfunction]
pub fn py_zmq_sub_discover(name: String, seeds: Vec<String>) -> PyResult<(PyStream, PyStream)> {
    let seed_refs: Vec<&str> = seeds.iter().map(|s| s.as_str()).collect();
    let (data, status) = zmq_sub::<Vec<u8>>((name.as_str(), SeedRegistry::new(&seed_refs)))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    Ok(streams_to_py(data, status))
}

/// Subscribe to a named ZMQ publisher via etcd service discovery.
///
/// Looks up `name` in etcd and connects to the publisher at the stored address.
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
#[pyfunction]
pub fn py_zmq_sub_etcd(name: String, endpoint: String) -> PyResult<(PyStream, PyStream)> {
    use wingfoil::adapters::etcd::EtcdConnection;
    use wingfoil::adapters::zmq::EtcdRegistry;
    let conn = EtcdConnection::new(endpoint);
    let (data, status) = zmq_sub::<Vec<u8>>((name.as_str(), EtcdRegistry::new(conn)))
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

/// Inner implementation for `zmq_pub_named` (seed-based registration).
pub fn py_zmq_pub_named_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    name: String,
    port: u16,
    seeds: Vec<String>,
) -> Rc<dyn Node> {
    let seed_refs: Vec<&str> = seeds.iter().map(|s| s.as_str()).collect();
    let registry = SeedRegistry::new(&seed_refs);
    let bytes_stream = py_bytes_stream(stream, "zmq_pub_named");
    bytes_stream.zmq_pub(port, (name.as_str(), registry))
}

/// Inner implementation for `zmq_pub_named_on` (seed-based, routable address).
pub fn py_zmq_pub_named_on_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    name: String,
    address: String,
    port: u16,
    seeds: Vec<String>,
) -> Rc<dyn Node> {
    let seed_refs: Vec<&str> = seeds.iter().map(|s| s.as_str()).collect();
    let registry = SeedRegistry::new(&seed_refs);
    let bytes_stream = py_bytes_stream(stream, "zmq_pub_named_on");
    bytes_stream.zmq_pub_on(&address, port, (name.as_str(), registry))
}

/// Inner implementation for `zmq_pub_etcd` (etcd-based registration).
pub fn py_zmq_pub_etcd_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    name: String,
    port: u16,
    endpoint: String,
) -> Rc<dyn Node> {
    use wingfoil::adapters::etcd::EtcdConnection;
    use wingfoil::adapters::zmq::EtcdRegistry;
    let registry = EtcdRegistry::new(EtcdConnection::new(endpoint));
    let bytes_stream = py_bytes_stream(stream, "zmq_pub_etcd");
    bytes_stream.zmq_pub(port, (name.as_str(), registry))
}

/// Inner implementation for `zmq_pub_etcd_on` (etcd-based, routable address).
pub fn py_zmq_pub_etcd_on_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    name: String,
    address: String,
    port: u16,
    endpoint: String,
) -> Rc<dyn Node> {
    use wingfoil::adapters::etcd::EtcdConnection;
    use wingfoil::adapters::zmq::EtcdRegistry;
    let registry = EtcdRegistry::new(EtcdConnection::new(endpoint));
    let bytes_stream = py_bytes_stream(stream, "zmq_pub_etcd_on");
    bytes_stream.zmq_pub_on(&address, port, (name.as_str(), registry))
}

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

/// Handle to a running ZMQ seed node. The seed stops when this object is deleted.
#[pyclass(unsendable, name = "SeedHandle")]
pub struct PySeedHandle(#[allow(dead_code)] SeedHandle);

#[pymethods]
impl PySeedHandle {
    fn __repr__(&self) -> &str {
        "SeedHandle(running)"
    }
}

/// Start a ZMQ seed node bound to `address` (e.g. `"tcp://0.0.0.0:7777"`).
///
/// The seed acts as a name registry: publishers register their address under a
/// name; subscribers query by name to find the publisher address. Only the
/// once-at-startup discovery handshake goes through the seed — data still flows
/// directly over PUB/SUB sockets.
///
/// The seed stops when the returned handle is deleted (or goes out of scope).
///
/// Args:
///     address: ZMQ endpoint to bind on (e.g. "tcp://0.0.0.0:7777")
///
/// Returns:
///     SeedHandle — drop to stop the seed
///
/// Raises:
///     RuntimeError: if the seed cannot bind to the given address
#[pyfunction]
pub fn py_start_seed(address: String) -> PyResult<PySeedHandle> {
    start_seed(&address)
        .map(PySeedHandle)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}
