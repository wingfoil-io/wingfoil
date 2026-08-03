//! Python bindings for the wingfoil **Prometheus** adapter
//! ([`wingfoil::adapters::prometheus`]).
//!
//! One class, no free functions:
//!
//! | Python                            | Rust                          |
//! |-----------------------------------|-------------------------------|
//! | `PrometheusExporter(addr)`        | [`PrometheusExporter::new`]   |
//! | `exporter.serve() -> port`        | [`PrometheusExporter::serve`] |
//! | `exporter.gauge(name, stream)`    | [`PrometheusSinkOps::prometheus_gauge`] |
//!
//! # Why this one is hand-written
//!
//! The exporter is a **stateful handle**: constructed, served, then registered
//! against repeatedly, with each registration returning a sink stream. That is
//! not a shape `#[pyadapter]` can generate — it emits free functions over a
//! `&GraphBuilder` or `&Stream<T>` receiver, with no handle form — so this is a
//! `#[pyclass]` with `#[pymethods]`, wiring through the same `PyStream` seams
//! (`typed_input`, `erased_output`) the macro uses. `fix_connect_tls` is the
//! other one; web's `WebServer` will be the third.
//!
//! # The dynamic edge
//!
//! The Rust sink is generic over any `Display` value and stringifies each tick.
//! The binding therefore takes the stream **erased** and renders with Python's
//! `str()` — the same contract, not a looser one.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Stringification fails loudly.** Legacy's `elem.str()…unwrap_or_default()`
//!    turned an object whose `__str__` raised into an *empty string*, which was
//!    then published as the metric's value. Here the run aborts naming the cause.
//! 2. **`register` is now `gauge`.** The engine calls the registration
//!    `prometheus_gauge`, and the metric type belongs in the name — legacy's
//!    `register` said nothing about what kind of metric it made, and leaves no
//!    room for a counter or histogram beside it.
//! 3. **Historical runs are a no-op**, inherited from the Rust adapter: a
//!    backtest never publishes fast-forwarded values to a live scrape endpoint,
//!    so a historical run's `/metrics` body stays empty. Legacy had no such
//!    guard.

use anyhow::{Result, anyhow};
use pyo3::prelude::*;
use wingfoil::adapters::prometheus::{PrometheusExporter, PrometheusSinkOps};
use wingfoil::prelude::{Stream, StreamOps};

use crate::PyElement;

/// Serves a Prometheus-compatible `GET /metrics` endpoint.
///
/// ```python
/// exporter = PrometheusExporter("127.0.0.1:0")
/// port = exporter.serve()
/// exporter.gauge("wingfoil_ticks_total", g.counter(period_nanos=1_000_000_000))
/// g.run(realtime=True, duration_nanos=5_000_000_000)
/// ```
///
/// The exporter owns no graph state — each `gauge` call returns an ordinary
/// sink `Stream` on the stream's own graph — so it may be created before or
/// after the `Graph`, and served before or after the metrics are registered.
#[pyclass(name = "PrometheusExporter", unsendable)]
pub struct PyPrometheusExporter {
    exporter: PrometheusExporter,
}

#[pymethods]
impl PyPrometheusExporter {
    /// Create an exporter that will bind `addr` on [`serve`](Self::serve), e.g.
    /// `"0.0.0.0:9091"`, or `"127.0.0.1:0"` for an OS-assigned port.
    #[new]
    fn new(addr: String) -> Self {
        Self {
            exporter: PrometheusExporter::new(addr),
        }
    }

    /// Start the HTTP server thread and return the port actually bound.
    ///
    /// The listener binds synchronously, so an address already in use raises
    /// here rather than failing silently once the graph is running. With port
    /// `0` in `addr`, the returned port is the one the OS assigned.
    fn serve(&self) -> PyResult<u16> {
        self.exporter
            .serve()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:#}")))
    }

    /// Register `stream` as a gauge named `name` and return the sink stream.
    ///
    /// Each tick's value is rendered with Python's `str()` and published as the
    /// metric's value; a value whose `__str__` raises aborts the run. `name`
    /// should follow Prometheus conventions, e.g. `wingfoil_ticks_total`.
    ///
    /// The returned terminal stream (value `None`) **must stay wired into the
    /// graph** — the metric only updates while that sink cycles. The sink is a
    /// no-op under a historical run, so a backtest never publishes its
    /// fast-forwarded values.
    fn gauge(&self, name: String, stream: PyRef<'_, crate::Stream>) -> crate::Stream {
        let object = stream.object();
        let text: Stream<String> = object.typed_input::<PyElement>().try_map(render);
        crate::Stream::from(object.erased_output::<()>(text.prometheus_gauge(&self.exporter, name)))
    }
}

/// Render one erased stream value with Python's `str()`.
///
/// Split out of the closure so the marshaling contract — including the two
/// failure modes — is unit-testable without standing up a graph.
fn render(elem: &PyElement) -> Result<String> {
    if elem.is_none() {
        return Err(anyhow!(
            "prometheus_gauge: stream value is empty (the upstream produced no value)"
        ));
    }
    Python::attach(|py| {
        elem.object()
            .bind(py)
            .str()
            .map(|s| s.to_string())
            .map_err(|e| anyhow!("prometheus_gauge: str() on the stream value raised: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::IntoPyObjectExt;

    #[test]
    fn a_value_renders_through_str() {
        Python::attach(|py| {
            let element = PyElement::new(1.5_f64.into_py_any(py).unwrap());
            assert_eq!("1.5", render(&element).unwrap());
        });
    }

    #[test]
    fn an_empty_value_errors() {
        let err = render(&PyElement::default()).unwrap_err();
        assert!(err.to_string().contains("stream value is empty"), "{err}");
    }

    #[test]
    fn a_raising_str_errors_rather_than_publishing_an_empty_string() {
        Python::attach(|py| {
            let raising = py
                .eval(
                    c"type('Bad', (), {'__str__': lambda self: (_ for _ in ()).throw(ValueError('nope'))})()",
                    None,
                    None,
                )
                .unwrap();
            let element = PyElement::new(raising.unbind());
            let err = render(&element).unwrap_err();
            assert!(
                err.to_string().contains("str() on the stream value raised"),
                "{err}"
            );
        });
    }
}
