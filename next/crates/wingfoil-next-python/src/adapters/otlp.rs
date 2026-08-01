//! Python bindings for the wingfoil-next **OTLP** adapter
//! ([`wingfoil_next::adapters::otlp`]).
//!
//! One graph entry point:
//!
//! | Python                   | Rust              | shape |
//! |--------------------------|-------------------|-------|
//! | `otlp_push(stream, …)`   | [`OtlpSinkOps`]   | push a gauge metric per tick |
//!
//! # The dynamic edge
//!
//! The Rust sink is generic over any `Display` value and stringifies each tick
//! before parsing it as `f64`. The binding therefore takes the stream **erased**
//! and stringifies via Python's `str()`, which is the same contract — a value
//! that does not render as a bare number is the caller's error, not a shape
//! mismatch.
//!
//! # Why the sink needed an engine change
//!
//! [`otlp_push`](OtlpSinkOps::otlp_push) took a `&'static str` metric name,
//! because the OTel SDK's gauge builder wants a `'static` name. A binding's name
//! arrives at runtime, so the legacy binding did
//! `Box::leak(metric_name.into_boxed_str())` — a deliberate leak per wiring
//! call. The SDK actually takes `impl Into<Cow<'static, str>>`, which an owned
//! `String` satisfies, so the bound was tighter than it needed to be: the trait
//! now takes `impl Into<Cow<'static, str>>` and the leak is gone. Every existing
//! `&'static str` caller is unaffected.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **No leak.** See above — legacy leaked the metric name on every
//!    `otlp_push` wiring call.
//! 2. **Stringification fails loudly.** Legacy's `elem.str()…unwrap_or_default()`
//!    turned an object whose `__str__` raised into an *empty string*, which then
//!    parsed as `0.0` and was published as real telemetry. Here the run aborts.
//! 3. **Historical runs are a no-op**, inherited from the Rust adapter: a
//!    backtest never publishes fast-forwarded values to a live endpoint. Legacy
//!    had no such guard.

use anyhow::{Result, anyhow};
use pyo3::prelude::*;
use wingfoil_next::adapters::otlp::{OtlpConfig, OtlpSinkOps};
use wingfoil_next::prelude::{Stream, StreamOps};

use crate::{PyElement, pyadapter};

/// Push each tick of this stream as an OTLP gauge metric.
///
/// Each value is rendered with Python's `str()` and parsed as a float, so the
/// stream should carry numbers (or objects whose `__str__` is a bare number). A
/// value whose `__str__` raises aborts the run; one that renders but does not
/// parse as a number records `0.0` and logs a warning, as the Rust adapter does.
///
/// `endpoint` is the OTLP HTTP endpoint (e.g. `"http://localhost:4318"`) and
/// `service_name` is reported in the resource attributes.
///
/// The sink is a **no-op under historical runs**, so a backtest never publishes
/// fast-forwarded values to a live endpoint. Returns a terminal stream whose
/// value is `None`; it must stay wired into the graph for anything to export.
#[pyadapter(name = otlp_push)]
#[pyo3(signature = (metric_name, endpoint, service_name))]
fn push(
    stream: &Stream<PyElement>,
    metric_name: String,
    endpoint: String,
    service_name: String,
) -> Result<Stream<()>> {
    let text: Stream<String> = stream.try_map(|elem: &PyElement| {
        if elem.is_none() {
            return Err(anyhow!(
                "otlp_push: stream value is empty (the upstream produced no value)"
            ));
        }
        Python::attach(|py| {
            elem.object()
                .bind(py)
                .str()
                .map(|s| s.to_string())
                .map_err(|e| anyhow!("otlp_push: str() on the stream value raised: {e}"))
        })
    });
    text.otlp_push(metric_name, OtlpConfig::new(endpoint, service_name))
}
