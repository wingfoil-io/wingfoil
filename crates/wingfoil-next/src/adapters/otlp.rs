//! otlp adapter — a realtime, push-based OpenTelemetry metrics **sink**: it
//! exports stream values as OTLP gauge metrics over HTTP/protobuf to any
//! OTLP-compatible backend (Grafana Alloy, Datadog, Honeycomb, New Relic, …).
//! It ports the metrics half of the legacy `wingfoil::adapters::otlp` module
//! onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `otlp` feature (it pulls in the OpenTelemetry Rust SDK plus the
//! `async` feature). Bring in what you need explicitly:
//!
//! - **Metrics sink** — the [`OtlpSinkOps`] extension trait on any `Stream<T>`
//!   whose values are [`Display`](std::fmt::Display), enabled with
//!   `use wingfoil_next::adapters::otlp::{OtlpConfig, OtlpSinkOps};`. Push a
//!   stream as a gauge with [`otlp_push`](OtlpSinkOps::otlp_push) and add the
//!   returned sink `Stream<()>` to the graph.
//! - **Trace/span sink** — the [`OtlpSpanOps`] extension trait on any
//!   `Stream<P>` whose values carry a latency record
//!   ([`HasLatency`](crate::latency::HasLatency)), enabled with
//!   `use wingfoil_next::adapters::otlp::{OtlpConfig, OtlpAttributeBuffer, OtlpSpanOps};`.
//!   Emit one parent span per tick plus one child span per stage hop with
//!   [`otlp_spans`](OtlpSpanOps::otlp_spans), routing high-cardinality
//!   per-request data (session IDs, trace IDs) to a tracing backend (Tempo,
//!   Jaeger, Honeycomb) without the Prometheus cardinality tax.
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
//! [`RunMode::HistoricalFrom`](wingfoil_next::RunMode::HistoricalFrom) the sink is a
//! no-op — no value is handed to the background task, so no meter provider is
//! built and **no network calls are made** — matching legacy, whose consumer
//! checked the run mode and drained without connecting. Next reads the run mode
//! from [`Ctx::run_mode`](crate::op::Ctx::run_mode) in the cycle itself, so the
//! same wiring runs deterministically in both modes.
//!
//! # Deviations from legacy
//!
//! Every legacy *metrics* capability (the OTLP HTTP/protobuf gauge export, the
//! `endpoint` / `service_name` config, the 500 ms periodic flush, the
//! non-numeric-records-`0.0` fallback, the historical no-op, provider-drop
//! flush) is preserved. The surface differs in three deliberate ways:
//!
//! 1. **The graph owns the tokio runtime.** Legacy hid a never-dropped global
//!    runtime inside its own `consume_async`; next's `GraphBuilder` owns one
//!    runtime, created lazily on first async use and dropped at teardown, shared
//!    by every async adapter — so [`otlp_push`](OtlpSinkOps::otlp_push) takes no
//!    `&Handle` (see `docs/runtime-ownership.md`; embed in your own runtime with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime)).
//!    The graph must be built, run, and dropped from a non-async thread (a
//!    `consume_async` footgun; see its docs).
//! 2. **The sink is an extension trait.** Legacy exposed an `OtlpPush` trait on
//!    `dyn Stream<T>`; next uses the sink-as-trait convention shared with
//!    [`prometheus`](crate::adapters::prometheus): `stream.otlp_push(name,
//!    config)` on a `Stream<T>`, returning the sink `Stream<()>`.
//! 3. **The trace/span exporter uses the same off-thread model.** Legacy's
//!    `OtlpSpans` looped over the source inside its own `consume_async`
//!    consumer, building the tracer provider once up front; next's
//!    [`consume_async`](crate::async_source::consume_async) is per-value, so
//!    [`otlp_spans`](OtlpSpanOps::otlp_spans) builds the provider lazily on the
//!    first exported value (kept alive until teardown) — the same lazy-build,
//!    provider-drop-flush shape as [`otlp_push`](OtlpSinkOps::otlp_push). Every
//!    legacy span capability is preserved: one parent span per tick, one child
//!    per stage hop, caller-supplied attributes via [`OtlpAttributeBuffer`], and
//!    the silent skip of all-zero / backwards timestamps. The argument order
//!    differs from legacy: next is
//!    [`otlp_spans`](OtlpSpanOps::otlp_spans)`(span_name, config, attrs)`
//!    (grouping the two `&'static str`-ish leading args before the config),
//!    whereas legacy was `otlp_spans(config, span_name, attrs)`.
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

use std::borrow::Cow;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::{
    Span as _, SpanKind, TraceContextExt as _, Tracer as _, TracerProvider as _,
};
use opentelemetry::{Context, KeyValue};
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use wingfoil_next::RunMode;

use crate::async_source::consume_async;
use crate::burst;
use crate::fluent::{Stream, StreamOps};
use crate::latency::{HasLatency, Latency};
use crate::op::{Activation, Ctx, Tick};

/// The OTel SDK export flush interval. A short interval ensures at least one
/// export happens during a short-running run; matches legacy's 500 ms.
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
/// `use`ing it enables `stream.otlp_push(name, config)` chaining,
/// layered over the [`register_op1`](crate::interp::Builder::register_op1)
/// primitive (for the run-mode guard) plus
/// [`consume_async`](crate::async_source::consume_async) (for the off-thread
/// export), the same way [`prometheus`](crate::adapters::prometheus) layers its
/// gauge sink.
pub trait OtlpSinkOps<T> {
    /// Push every tick of this stream as an OTLP gauge metric named
    /// `metric_name`, exporting to `config`'s endpoint off the graph thread.
    ///
    /// `metric_name` accepts a `&'static str` or an owned `String` — the OTel
    /// SDK's gauge builder takes a `Cow<'static, str>`, so a name computed at
    /// runtime (as a dynamic caller such as the Python binding has) needs no
    /// leak. Values are converted to `f64` via
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
    /// [`RunMode::HistoricalFrom`](wingfoil_next::RunMode::HistoricalFrom): no value
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
        metric_name: impl Into<Cow<'static, str>>,
        config: impl Into<OtlpConfig>,
    ) -> Result<Stream<()>>;
}

impl<T> OtlpSinkOps<T> for Stream<T>
where
    T: std::fmt::Display + Clone + Default + Send + 'static,
{
    fn otlp_push(
        &self,
        metric_name: impl Into<Cow<'static, str>>,
        config: impl Into<OtlpConfig>,
    ) -> Result<Stream<()>> {
        let config = config.into();
        let metric_name = metric_name.into();

        // The off-thread export. `consume_async`'s consumer task runs on the
        // graph's runtime, so building the `rt-tokio` PeriodicReader and
        // recording on the gauge both happen inside a tokio context. The meter
        // provider is built lazily on the first value (kept alive in `provider`
        // so it flushes when this closure — and thus the consumer task — is
        // dropped at teardown), never in historical mode (no value reaches here).
        let mut gauge: Option<opentelemetry::metrics::Gauge<f64>> = None;
        let mut provider: Option<SdkMeterProvider> = None;
        let (sink, flush) = consume_async(&self.graph(), None, move |value: T| {
            let result = build_and_record(
                &mut gauge,
                &mut provider,
                metric_name.as_ref(),
                &config,
                &value.to_string(),
            );
            async move { result }
        })?;

        // The run-mode guard rides `register_op1` (`for_each` cannot see the
        // `Ctx`). In historical mode the value is never handed to `consume_async`,
        // so the background task stays idle and makes no network calls — the
        // prometheus-sink pattern. The `flush` teardown drains the consumer at
        // run end and surfaces a final-cycle export error.
        let sink_stream = self.wire(move |b, h| {
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
        });
        Ok(sink_stream.finally(flush))
    }
}

/// Build the meter provider + gauge on first use (kept alive in `provider`), then
/// record `value_str` parsed as `f64`. Runs inside the `consume_async` task's
/// tokio context. A build error aborts the run (surfaced on a later cycle, per
/// `consume_async`); a non-numeric value records `0.0` with a warning.
fn build_and_record(
    gauge: &mut Option<opentelemetry::metrics::Gauge<f64>>,
    provider: &mut Option<SdkMeterProvider>,
    metric_name: &str,
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
        *gauge = Some(meter.f64_gauge(metric_name.to_string()).build());
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

// ---------------------------------------------------------------------------
// Trace/span sink
// ---------------------------------------------------------------------------

/// Reusable buffer for span attributes in [`OtlpSpanOps::otlp_spans`]. Avoids
/// allocating a fresh `Vec<KeyValue>` per tick by pre-allocating once and
/// reusing it — the attribute closure fills it via [`add`](Self::add).
pub struct OtlpAttributeBuffer {
    attrs: Vec<KeyValue>,
}

impl OtlpAttributeBuffer {
    /// Add a key-value attribute. The buffer is reused across ticks, so this is
    /// efficient even at high tick rates.
    pub fn add(&mut self, key: &'static str, value: impl Into<opentelemetry::Value>) {
        self.attrs.push(KeyValue::new(key, value));
    }

    fn clear(&mut self) {
        self.attrs.clear();
    }

    fn take(&mut self) -> Vec<KeyValue> {
        std::mem::take(&mut self.attrs)
    }
}

impl Default for OtlpAttributeBuffer {
    fn default() -> Self {
        OtlpAttributeBuffer {
            attrs: Vec::with_capacity(8),
        }
    }
}

/// A realtime, push-based OpenTelemetry **trace** sink — an extension trait on
/// any `Stream<P>` whose values carry a latency record
/// ([`HasLatency`](crate::latency::HasLatency)).
///
/// `use`ing it enables `stream.otlp_spans(name, config, attrs)` chaining,
/// layered over the same
/// [`register_op1`](crate::interp::Builder::register_op1) run-mode guard plus
/// [`consume_async`](crate::async_source::consume_async) off-thread export as
/// [`OtlpSinkOps`].
pub trait OtlpSpanOps<P>
where
    P: HasLatency,
{
    /// Emit one parent span per upstream tick covering the full
    /// `stamps[0] .. stamps[N-1]` interval, plus one child span per adjacent
    /// stage pair (named `"<prev_stage>__<next_stage>"`), exporting to
    /// `config`'s endpoint off the graph thread.
    ///
    /// The `attrs` closure is called once per tick to populate per-payload
    /// attributes on the parent span via [`OtlpAttributeBuffer::add`]; the
    /// buffer is pre-allocated and reused. Spans whose stamps are all zero, or
    /// whose endpoints go backwards, are silently skipped, so a partially
    /// stamped record never corrupts the trace.
    ///
    /// - `span_name`: the parent span name (a `&'static str`).
    /// - `config`: the OTLP endpoint and service name (accepts an
    ///   [`OtlpConfig`], or a `(endpoint, service_name)` tuple).
    ///
    /// The graph owns the tokio runtime (see the module docs — the graph must be
    /// driven from a non-async thread).
    ///
    /// The sink is a **no-op** under
    /// [`RunMode::HistoricalFrom`](wingfoil_next::RunMode::HistoricalFrom): no span
    /// is exported and no tracer provider is built.
    ///
    /// The returned `Stream<()>` **must** be added to the graph for spans to be
    /// exported.
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time only if the graph's async runtime cannot be
    /// created.
    #[must_use = "otlp_spans returns a sink Stream that must be added to the graph"]
    fn otlp_spans<F>(
        &self,
        span_name: &'static str,
        config: impl Into<OtlpConfig>,
        attrs: F,
    ) -> Result<Stream<()>>
    where
        F: Fn(&P, &mut OtlpAttributeBuffer) + Send + Sync + 'static;
}

impl<P> OtlpSpanOps<P> for Stream<P>
where
    P: HasLatency + Clone + Default + Send + 'static,
{
    fn otlp_spans<F>(
        &self,
        span_name: &'static str,
        config: impl Into<OtlpConfig>,
        attrs: F,
    ) -> Result<Stream<()>>
    where
        F: Fn(&P, &mut OtlpAttributeBuffer) + Send + Sync + 'static,
    {
        let config = config.into();

        // Stage layout is compile-time constant, so precompute the hop names
        // once here rather than per tick.
        let n = P::L::N;
        let stage_names = P::L::stage_names();
        let hop_names: Vec<String> = (1..n)
            .map(|i| format!("{}__{}", stage_names[i - 1], stage_names[i]))
            .collect();

        // Built lazily on the first exported value (inside the consumer task's
        // tokio context), kept alive so the batch exporter flushes when the
        // task is dropped at teardown — never in historical mode (no value
        // reaches here). Mirrors the otlp_push lazy-build/provider-drop shape.
        let mut tracer: Option<opentelemetry_sdk::trace::Tracer> = None;
        let mut provider: Option<SdkTracerProvider> = None;
        let mut attr_buffer = OtlpAttributeBuffer::default();
        let (sink, flush) = consume_async(&self.graph(), None, move |value: P| {
            let result = build_and_emit(
                &mut tracer,
                &mut provider,
                &config,
                span_name,
                n,
                &hop_names,
                &attrs,
                &mut attr_buffer,
                &value,
            );
            async move { result }
        })?;

        let sink_stream = self.wire(move |b, h| {
            b.register_op1(
                h,
                "otlp_spans",
                Activation::NONE,
                sink,
                || (),
                move |sink: &mut _, _state: &mut (), value: &P, ctx: &mut Ctx<'_>| {
                    if matches!(ctx.run_mode(), RunMode::HistoricalFrom(_)) {
                        return Ok(Tick::Value(()));
                    }
                    sink(&burst![value.clone()])?;
                    Ok(Tick::Value(()))
                },
            )
        });
        Ok(sink_stream.finally(flush))
    }
}

/// Build the tracer provider on first use (kept alive in `provider`), then emit
/// a parent span + one child per stamped hop for `value`. Runs inside the
/// `consume_async` task's tokio context. A build error aborts the run (surfaced
/// on a later cycle, per `consume_async`).
#[allow(clippy::too_many_arguments)]
fn build_and_emit<P, F>(
    tracer: &mut Option<opentelemetry_sdk::trace::Tracer>,
    provider: &mut Option<SdkTracerProvider>,
    config: &OtlpConfig,
    span_name: &'static str,
    n: usize,
    hop_names: &[String],
    attrs: &F,
    attr_buffer: &mut OtlpAttributeBuffer,
    value: &P,
) -> anyhow::Result<()>
where
    P: HasLatency,
    F: Fn(&P, &mut OtlpAttributeBuffer),
{
    if tracer.is_none() {
        let exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(&config.endpoint)
            .build()
            .map_err(|e| anyhow::anyhow!("otlp_spans: failed to build exporter: {e}"))?;
        let resource = Resource::builder_empty()
            .with_service_name(config.service_name.clone())
            .build();
        let built = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build();
        *tracer = Some(built.tracer("wingfoil"));
        *provider = Some(built);
    }
    let tracer = tracer.as_ref().expect("invariant: tracer built above");
    emit_spans(tracer, value, attrs, span_name, n, hop_names, attr_buffer);
    Ok(())
}

/// Emit the parent + child spans for one value. Skips the whole trace if the
/// full-journey endpoints are unstamped or backwards, and skips individual hops
/// the same way, so a partially stamped record still produces the hops that
/// were stamped.
fn emit_spans<P, F>(
    tracer: &opentelemetry_sdk::trace::Tracer,
    value: &P,
    attrs: &F,
    span_name: &'static str,
    n: usize,
    hop_names: &[String],
    attr_buffer: &mut OtlpAttributeBuffer,
) where
    P: HasLatency,
    F: Fn(&P, &mut OtlpAttributeBuffer),
{
    let stamps = value.latency().stamps();
    // A trace needs at least one real start/end pair.
    if n < 2 || stamps[0] == 0 || stamps[n - 1] == 0 || stamps[n - 1] < stamps[0] {
        return;
    }

    attr_buffer.clear();
    attrs(value, attr_buffer);

    let parent = tracer
        .span_builder(span_name)
        .with_kind(SpanKind::Server)
        .with_start_time(ns_to_system_time(stamps[0]))
        .with_attributes(attr_buffer.take())
        .start(tracer);
    let cx = Context::current_with_span(parent);

    for i in 1..n {
        let a = stamps[i - 1];
        let b = stamps[i];
        if a == 0 || b == 0 || b < a {
            continue;
        }
        let mut child = tracer
            .span_builder(hop_names[i - 1].clone())
            .with_kind(SpanKind::Internal)
            .with_start_time(ns_to_system_time(a))
            .start_with_context(tracer, &cx);
        child.end_with_timestamp(ns_to_system_time(b));
    }

    cx.span()
        .end_with_timestamp(ns_to_system_time(stamps[n - 1]));
}

fn ns_to_system_time(ns: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(ns)
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
