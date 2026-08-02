//! Python bindings for the otlp adapter.

use crate::PyNode;
use crate::py_stream::PyStream;
use pyo3::prelude::*;
use wingfoil::StreamOperators;
use wingfoil::adapters::otlp::{OtlpConfig, OtlpPush};

/// Inner implementation for the `.otlp_push()` stream method.
pub fn py_otlp_push_inner(
    stream: &PyStream,
    metric_name: String,
    endpoint: String,
    service_name: String,
) -> PyNode {
    let config = OtlpConfig {
        endpoint,
        service_name,
    };
    let str_stream = stream.0.map(|elem| {
        Python::attach(|py| {
            elem.as_ref()
                .bind(py)
                .str()
                .map(|s| s.to_string())
                .unwrap_or_default()
        })
    });
    let metric_name_static = Box::leak(metric_name.into_boxed_str());
    PyNode::new(str_stream.otlp_push(metric_name_static, config))
}
