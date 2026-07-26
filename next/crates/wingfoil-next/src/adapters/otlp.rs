//! otlp adapter — a realtime, push-based OpenTelemetry metrics **sink**: it
//! exports stream values as OTLP gauge metrics over HTTP/protobuf to any
//! OTLP-compatible backend (Grafana Alloy, Datadog, Honeycomb, New Relic, …).
//! It ports the metrics half of the classic `wingfoil::adapters::otlp` module
//! onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `otlp` feature (it pulls in the OpenTelemetry Rust SDK plus the
//! `async` feature). Bring in what you need explicitly:
//!
//! - **Sink** — the [`OtlpSinkOps`] extension trait on any `Stream<T>` whose
//!   values are [`Display`](std::fmt::Display), enabled with
//!   `use wingfoil_next::adapters::otlp::{OtlpConfig, OtlpSinkOps};`. Push a
//!   stream as a gauge with [`otlp_push`](OtlpSinkOps::otlp_push) and add the
//!   returned sink `Stream<()>` to the graph.
//!
//! # Sink (the off-thread export model)
//!
//! Each tick's value is stringified and parsed to `f64`, then recorded on an
//! OpenTelemetry `f64` gauge. The recording — and the whole OTel SDK export
//! machinery (an HTTP/protobuf [`MetricExporter`] driven by a 500 ms
//! `rt-tokio` [`PeriodicReader`]) — runs **off the graph thread** via
//! [`consume_async`](crate::async_source::consume_async): the sink hands each
//! value to a background tokio task, so a slow or blocked export never stalls
//! the single-threaded engine. The meter provider is built once, on the first
//! exported value, inside that task's runtime context; it is **dropped** at
//! graph teardown, which flushes the final batch (explicit `shutdown()` is
//! deliberately avoided — its timeout can fail when the async runtime is
//! unavailable; see opentelemetry-rust issue #3137).
//!
//! Values whose [`Display`](std::fmt::Display) output is not a plain number
//! (e.g. `"42 units"`) record `0.0` and emit a `log::warn`; `.map()` upstream
//! to extract a numeric field if your type does not format as a bare number.
//!
//! # Historical / backtesting mode
//!
//! Telemetry is a **realtime** concept. Under
//! [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom) the sink is a
//! no-op — no value is handed to the background task, so no meter provider is
//! built and **no network calls are made** — matching classic, whose consumer
//! checked the run mode and drained without connecting. Next reads the run mode
//! from [`Ctx::run_mode`](crate::op::Ctx::run_mode) in the cycle itself, so the
//! same wiring runs deterministically in both modes.
//!
//! # Deviations from classic
//!
//! Every classic *metrics* capability (the OTLP HTTP/protobuf gauge export, the
//! `endpoint` / `service_name` config, the 500 ms periodic flush, the
//! non-numeric-records-`0.0` fallback, the historical no-op, provider-drop
//! flush) is preserved. The surface differs in three deliberate ways:
//!
//! 1. **The graph owns the tokio runtime.** Classic hid a never-dropped global
//!    runtime inside its own `consume_async`; next's `GraphBuilder` owns one
//!    runtime, created lazily on first async use and dropped at teardown, shared
//!    by every async adapter — so [`otlp_push`](OtlpSinkOps::otlp_push) takes no
//!    `&Handle` (see `docs/runtime-ownership.md`; embed in your own runtime with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime)).
//!    The graph must be built, run, and dropped from a non-async thread (a
//!    `consume_async` footgun; see its docs).
//! 2. **The sink is an extension trait.** Classic exposed an `OtlpPush` trait on
//!    `dyn Stream<T>`; next uses the sink-as-trait convention shared with
//!    [`prometheus`](crate::adapters::prometheus): `stream.otlp_push(name,
//!    config)` on a `Stream<T>`, returning the sink `Stream<()>`.
//! 3. **The trace/span exporter is not yet ported.** Classic's `OtlpSpans` emits
//!    OpenTelemetry spans from `Stream<P: HasLatency>` values. That path depends
//!    on the `Traced` / `HasLatency` / `latency_stages!` latency infrastructure,
//!    which has **not** been ported to next (a separate roadmap item — see
//!    `next/docs/port-plan.md`). Until that lands, only the metrics push is
//!    available here; the span export is a tracked capability gap. `otlp_push`
//!    itself is fully at parity.
//!
//! # Setup (integration test)
//!
//! The self-contained integration test starts an OTel collector via
//! testcontainers — no manual Docker setup required. To run the example against
//! a collector:
//!
//! ```sh
//! docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0
//! ```

use std::time::Duration;

use anyhow::Result;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_otlp::{MetricExporter, WithExportConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use wingfoil::RunMode;

use crate::async_source::consume_async;
use crate::burst;
use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Tick};

/// The OTel SDK export flush interval. A short interval ensures at least one
/// export happens during a short-running run; matches classic's 500 ms.
const EXPORT_INTERVAL: Duration = Duration::from_millis(500);

/// Connection configuration for an OTLP metrics endpoint.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    /// OTLP HTTP endpoint, e.g. `"http://localhost:4318"`.
    pub endpoint: String,
    /// Service name reported in OTLP resource attributes.
    pub service_name: String,
}

impl OtlpConfig {
    /// Create a config from an endpoint and a service name.
    pub fn new(endpoint: impl Into<String>, service_name: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            service_name: service_name.into(),
        }
    }
}

impl From<(&str, &str)> for OtlpConfig {
    fn from((endpoint, service_name): (&str, &str)) -> Self {
        Self::new(endpoint, service_name)
    }
}

impl From<(String, String)> for OtlpConfig {
    fn from((endpoint, service_name): (String, String)) -> Self {
        Self::new(endpoint, service_name)
    }
}

/// A realtime, push-based OpenTelemetry metrics sink — an extension trait on any
/// `Stream<T>` whose values are [`Display`](std::fmt::Display).
///
/// `use`ing it enables `stream.otlp_push(&handle, name, config)` chaining,
/// layered over the [`register_op1`](crate::interp::Builder::register_op1)
/// primitive (for the run-mode guard) plus
/// [`consume_async`](crate::async_source::consume_async) (for the off-thread
/// export), the same way [`prometheus`](crate::adapters::prometheus) layers its
/// gauge sink.
pub trait OtlpSinkOps<T> {
    /// Push every tick of this stream as an OTLP gauge metric named
    /// `metric_name`, exporting to `config`'s endpoint off the graph thread.
    ///
    /// `metric_name` must be a `&'static str` (the OTel SDK's `f64_gauge`
    /// builder requires a `'static` name). Values are converted to `f64` via
    /// `T::to_string().parse::<f64>()`; a value that does not format as a bare
    /// number records `0.0` and emits a `log::warn`.
    ///
    /// - `config`: the OTLP endpoint and service name (accepts an
    ///   [`OtlpConfig`], or a `(endpoint, service_name)` tuple).
    ///
    /// The graph owns the tokio runtime (see the module docs — the graph must be
    /// driven from a non-async thread).
    ///
    /// The sink is a **no-op** under
    /// [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom): no value
    /// is exported and no meter provider is built, so a backtest never publishes
    /// fast-forwarded values to a live endpoint.
    ///
    /// The returned `Stream<()>` **must** be added to the graph (or built and
    /// run) for metrics to be exported.
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time only if the graph's async runtime cannot be
    /// created.
    #[must_use = "otlp_push returns a sink Stream that must be added to the graph"]
    fn otlp_push(
        &self,
        metric_name: &'static str,
        config: impl Into<OtlpConfig>,
    ) -> Result<Stream<()>>;
}

impl<T> OtlpSinkOps<T> for Stream<T>
where
    T: std::fmt::Display + Clone + Default + Send + 'static,
{
    fn otlp_push(
        &self,
        metric_name: &'static str,
        config: impl Into<OtlpConfig>,
    ) -> Result<Stream<()>> {
        let config = config.into();

        // The off-thread export. `consume_async`'s consumer task runs on the
        // graph's runtime, so building the `rt-tokio` PeriodicReader and
        // recording on the gauge both happen inside a tokio context. The meter
        // provider is built lazily on the first value (kept alive in `provider`
        // so it flushes when this closure — and thus the consumer task — is
        // dropped at teardown), never in historical mode (no value reaches here).
        let mut gauge: Option<opentelemetry::metrics::Gauge<f64>> = None;
        let mut provider: Option<SdkMeterProvider> = None;
        let sink = consume_async(&self.graph(), None, move |value: T| {
            let result = build_and_record(
                &mut gauge,
                &mut provider,
                metric_name,
                &config,
                &value.to_string(),
            );
            async move { result }
        })?;

        // The run-mode guard rides `register_op1` (`for_each` cannot see the
        // `Ctx`). In historical mode the value is never handed to `consume_async`,
        // so the background task stays idle and makes no network calls — the
        // prometheus-sink pattern.
        Ok(self.wire(move |b, h| {
            b.register_op1(
                h,
                "otlp_push",
                Activation::NONE,
                sink,
                || (),
                move |sink: &mut _, _state: &mut (), value: &T, ctx: &mut Ctx<'_>| {
                    if matches!(ctx.run_mode(), RunMode::HistoricalFrom(_)) {
                        return Ok(Tick::Value(()));
                    }
                    sink(&burst![value.clone()])?;
                    Ok(Tick::Value(()))
                },
            )
        }))
    }
}

/// Build the meter provider + gauge on first use (kept alive in `provider`), then
/// record `value_str` parsed as `f64`. Runs inside the `consume_async` task's
/// tokio context. A build error aborts the run (surfaced on a later cycle, per
/// `consume_async`); a non-numeric value records `0.0` with a warning.
fn build_and_record(
    gauge: &mut Option<opentelemetry::metrics::Gauge<f64>>,
    provider: &mut Option<SdkMeterProvider>,
    metric_name: &'static str,
    config: &OtlpConfig,
    value_str: &str,
) -> anyhow::Result<()> {
    if gauge.is_none() {
        let exporter = MetricExporter::builder()
            .with_http()
            .with_endpoint(&config.endpoint)
            .build()
            .map_err(|e| anyhow::anyhow!("otlp_push: failed to build exporter: {e}"))?;
        let reader = PeriodicReader::builder(exporter)
            .with_interval(EXPORT_INTERVAL)
            .build();
        let resource = Resource::builder_empty()
            .with_service_name(config.service_name.clone())
            .build();
        let built = SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(resource)
            .build();
        let meter = built.meter("wingfoil");
        *gauge = Some(meter.f64_gauge(metric_name).build());
        *provider = Some(built);
    }
    let v: f64 = value_str.parse().unwrap_or_else(|_| {
        log::warn!("otlp_push: could not parse {value_str:?} as f64, recording 0.0");
        0.0
    });
    gauge
        .as_ref()
        .expect("invariant: gauge built above")
        .record(v, &[]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::OtlpConfig;

    #[test]
    fn config_from_str_tuple_and_new() {
        let a = OtlpConfig::new("http://localhost:4318", "svc");
        assert_eq!(a.endpoint, "http://localhost:4318");
        assert_eq!(a.service_name, "svc");

        let b: OtlpConfig = ("http://localhost:4318", "svc").into();
        assert_eq!(b.endpoint, a.endpoint);
        assert_eq!(b.service_name, a.service_name);
    }
}
