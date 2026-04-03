//! Python bindings for the prometheus adapter.

use crate::PyNode;
use crate::py_stream::PyStream;
use pyo3::prelude::*;
use wingfoil::StreamOperators;
use wingfoil::adapters::prometheus::PrometheusExporter;

/// Serves a Prometheus-compatible GET /metrics endpoint.
///
/// Usage:
///   exporter = PrometheusExporter("0.0.0.0:9091")
///   port = exporter.serve()
///   node = exporter.register("my_metric", stream)
#[pyclass(unsendable, name = "PrometheusExporter")]
pub struct PyPrometheusExporter {
    inner: PrometheusExporter,
}

#[pymethods]
impl PyPrometheusExporter {
    #[new]
    fn new(addr: String) -> Self {
        Self {
            inner: PrometheusExporter::new(addr),
        }
    }

    /// Start the HTTP server. Returns the port that was bound.
    fn serve(&self) -> PyResult<u16> {
        self.inner
            .serve()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Register a stream as a named gauge metric. Returns a Node.
    fn register(&self, name: String, stream: &PyStream) -> PyNode {
        let str_stream = stream.0.map(|elem| {
            Python::attach(|py| {
                elem.as_ref()
                    .bind(py)
                    .str()
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            })
        });
        PyNode::new(self.inner.register(name, str_stream))
    }
}
