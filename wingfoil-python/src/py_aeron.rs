//! Python bindings for the Aeron adapter.
//!
//! Aeron is addressed by `(channel, stream_id)` and **requires a running media
//! driver**: unlike the string-addressed adapters (zmq, etcd, kafka), the
//! subscriber/publisher handles are obtained from `AeronHandle::connect()` at
//! construction time, so `py_aeron_sub` / `_inner` connect eagerly and raise on
//! failure rather than deferring to graph run.
//!
//! The wire format is raw `bytes` (like zmq): the subscriber yields `list[bytes]`
//! per tick and `aeron_pub` publishes each `bytes` value as one message.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use std::rc::Rc;
use std::time::Duration;
use wingfoil::adapters::aeron::{
    AeronHandle, AeronMode, AeronPub, AeronSubOptions, FragmentBuffer, TransportError,
    aeron_sub_fragment,
};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Polling strategy for the Aeron subscriber.
#[pyclass(eq, eq_int, name = "AeronMode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyAeronMode {
    /// Poll inside the graph `cycle()` on the graph thread — lowest latency.
    Spin,
    /// Poll on a background thread, deliver via channel — frees the graph thread.
    Threaded,
}

impl From<PyAeronMode> for AeronMode {
    fn from(m: PyAeronMode) -> Self {
        match m {
            PyAeronMode::Spin => AeronMode::Spin,
            PyAeronMode::Threaded => AeronMode::Threaded,
        }
    }
}

fn connect_timeout(timeout_secs: f64) -> Duration {
    Duration::from_secs_f64(timeout_secs)
}

/// Subscribe to an Aeron channel.
///
/// Each tick yields a `list[bytes]` containing all messages received in that
/// graph cycle.
///
/// Requires a running Aeron media driver — connection happens at call time and
/// raises `RuntimeError` if the driver is unreachable or the subscription
/// cannot be resolved within `timeout_secs`.
///
/// Args:
///     channel: Aeron channel URI, e.g. `"aeron:ipc"` or `"aeron:udp?endpoint=host:40123"`
///     stream_id: Aeron stream id
///     mode: Polling mode (`AeronMode.Spin` or `AeronMode.Threaded`)
///     timeout_secs: How long to wait for the subscription to resolve
#[pyfunction]
#[pyo3(signature = (channel, stream_id, mode=PyAeronMode::Spin, timeout_secs=5.0))]
pub fn py_aeron_sub(
    channel: String,
    stream_id: i32,
    mode: PyAeronMode,
    timeout_secs: f64,
) -> PyResult<PyStream> {
    let handle = AeronHandle::connect()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    let subscriber = handle
        .subscription(&channel, stream_id, connect_timeout(timeout_secs))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    let data: Rc<dyn Stream<Burst<Vec<u8>>>> = aeron_sub_fragment(
        subscriber,
        |f: &FragmentBuffer<'_>| -> Result<Option<Vec<u8>>, TransportError> {
            Ok(Some(f.as_ref().to_vec()))
        },
        AeronSubOptions {
            mode: mode.into(),
            ..Default::default()
        },
    );

    let data_py = data.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|b| PyBytes::new(py, &b).into_any().unbind())
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    Ok(PyStream(data_py))
}

/// Inner implementation for the `.aeron_pub()` stream method.
///
/// The stream must yield `bytes`; each value is published as one Aeron message.
/// Connects at call time and raises `RuntimeError` on failure.
pub fn py_aeron_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    channel: String,
    stream_id: i32,
    timeout_secs: f64,
) -> PyResult<Rc<dyn Node>> {
    let handle = AeronHandle::connect()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    let publisher = handle
        .publication(&channel, stream_id, connect_timeout(timeout_secs))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    let bytes_stream: Rc<dyn Stream<Vec<u8>>> = stream.map(|elem| {
        Python::attach(|py| {
            elem.as_ref().extract::<Vec<u8>>(py).unwrap_or_else(|e| {
                log::error!("aeron_pub: stream value is not bytes: {e}");
                Vec::new()
            })
        })
    });

    // Wrap each single value into a one-element burst, then publish as-is.
    // `Burst<T>` is a `TinyVec<[T; 1]>`, which implements `FromIterator`.
    let burst_stream: Rc<dyn Stream<Burst<Vec<u8>>>> =
        bytes_stream.map(|b| std::iter::once(b).collect::<Burst<Vec<u8>>>());
    Ok(burst_stream.aeron_pub(publisher, |b: &Vec<u8>| b.clone()))
}
