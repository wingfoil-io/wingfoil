//! Python bindings for the FIX protocol adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use wingfoil::StreamOperators;
use wingfoil::adapters::fix::{
    FixMessage, FixPollMode, FixSessionStatus, fix_accept, fix_connect, fix_connect_tls,
};

/// Connect to a FIX acceptor and return `(data_stream, status_stream)`.
///
/// Args:
///     host: FIX acceptor hostname
///     port: FIX acceptor port
///     sender_comp_id: SenderCompID for this session
///     target_comp_id: TargetCompID of the counterparty
///
/// Returns:
///     A tuple of `(data_stream, status_stream)` where data_stream yields
///     lists of message dicts and status_stream yields session status strings.
#[pyfunction]
pub fn py_fix_connect(
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
) -> (PyStream, PyStream) {
    let (data, status) = fix_connect(
        &host,
        port,
        &sender_comp_id,
        &target_comp_id,
        FixPollMode::Threaded,
    );

    let py_data = data.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst.into_iter().map(|msg| msg_to_py(py, &msg)).collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    let py_status = status.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst.into_iter().map(|s| status_to_py(py, &s)).collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    (PyStream(py_data), PyStream(py_status))
}

/// Connect to a TLS-secured FIX acceptor (e.g. LMAX) and return
/// `(data_stream, status_stream, injector_func)`.
///
/// The injector is a callable that sends a FIX message dict on the session:
/// ```python
/// injector({"msg_type": "V", "fields": [(262, "req1"), (263, "1")]})
/// ```
///
/// Args:
///     host: FIX acceptor hostname
///     port: FIX acceptor port (typically 443 for TLS)
///     sender_comp_id: SenderCompID (usually your username)
///     target_comp_id: TargetCompID of the counterparty
///     password: Optional password (tag 554 in Logon)
#[pyfunction]
#[pyo3(signature = (host, port, sender_comp_id, target_comp_id, password=None))]
pub fn py_fix_connect_tls(
    host: String,
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
    password: Option<String>,
) -> (PyStream, PyStream, Py<PyAny>) {
    let (data, status, injector) = fix_connect_tls(
        &host,
        port,
        &sender_comp_id,
        &target_comp_id,
        password.as_deref(),
    );

    let py_data = data.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst.into_iter().map(|msg| msg_to_py(py, &msg)).collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    let py_status = status.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst.into_iter().map(|s| status_to_py(py, &s)).collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    let inject_fn = Python::attach(|py| {
        let injector_cls = PyFixInjector { injector };
        Py::new(py, injector_cls).unwrap().into_any()
    });

    (PyStream(py_data), PyStream(py_status), inject_fn)
}

/// Bind a FIX acceptor and return `(data_stream, status_stream)`.
///
/// Args:
///     port: Port to listen on
///     sender_comp_id: SenderCompID for this session
///     target_comp_id: Expected TargetCompID of the connecting initiator
#[pyfunction]
pub fn py_fix_accept(
    port: u16,
    sender_comp_id: String,
    target_comp_id: String,
) -> (PyStream, PyStream) {
    let (data, status) = fix_accept(
        port,
        &sender_comp_id,
        &target_comp_id,
        FixPollMode::Threaded,
    );

    let py_data = data.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst.into_iter().map(|msg| msg_to_py(py, &msg)).collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    let py_status = status.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst.into_iter().map(|s| status_to_py(py, &s)).collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });

    (PyStream(py_data), PyStream(py_status))
}

/// Python wrapper around [`FixInjector`] for sending outbound FIX messages.
#[pyclass(unsendable)]
struct PyFixInjector {
    injector: wingfoil::adapters::fix::FixInjector,
}

#[pymethods]
impl PyFixInjector {
    /// Send a FIX message on the session.
    ///
    /// Args:
    ///     msg: A dict with "msg_type" (str) and "fields" (list of [tag, value] pairs).
    fn inject(&self, msg: &Bound<'_, PyDict>) -> PyResult<()> {
        let msg_type = msg
            .get_item("msg_type")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'msg_type'"))?
            .extract::<String>()?;

        let fields_obj = msg
            .get_item("fields")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'fields'"))?;
        let fields_list = fields_obj.cast::<PyList>()?;

        let mut fields = Vec::new();
        for item in fields_list.iter() {
            let pair = item.cast::<PyList>()?;
            if pair.len() != 2 {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "each field must be [tag, value]",
                ));
            }
            let tag: u32 = pair.get_item(0)?.extract()?;
            let value: String = pair.get_item(1)?.extract()?;
            fields.push((tag, value));
        }

        self.injector.inject(FixMessage {
            msg_type,
            seq_num: 0,
            sending_time: wingfoil::NanoTime::ZERO,
            fields,
        });
        Ok(())
    }
}

fn msg_to_py(py: Python<'_>, msg: &FixMessage) -> Py<PyAny> {
    let dict = PyDict::new(py);
    dict.set_item("msg_type", &msg.msg_type).unwrap();
    dict.set_item("seq_num", msg.seq_num).unwrap();
    let fields: Vec<(u32, &str)> = msg.fields.iter().map(|(t, v)| (*t, v.as_str())).collect();
    dict.set_item("fields", fields).unwrap();
    dict.into_any().unbind()
}

fn status_to_py(py: Python<'_>, status: &FixSessionStatus) -> Py<PyAny> {
    match status {
        FixSessionStatus::Disconnected => "disconnected"
            .into_pyobject(py)
            .unwrap()
            .into_any()
            .unbind(),
        FixSessionStatus::LoggingIn => "logging_in".into_pyobject(py).unwrap().into_any().unbind(),
        FixSessionStatus::LoggedIn => "logged_in".into_pyobject(py).unwrap().into_any().unbind(),
        FixSessionStatus::LoggedOut(reason) => {
            let dict = PyDict::new(py);
            dict.set_item("status", "logged_out").unwrap();
            dict.set_item("reason", reason.as_deref().unwrap_or(""))
                .unwrap();
            dict.into_any().unbind()
        }
        FixSessionStatus::Error(msg) => {
            let dict = PyDict::new(py);
            dict.set_item("status", "error").unwrap();
            dict.set_item("message", msg.as_str()).unwrap();
            dict.into_any().unbind()
        }
    }
}
