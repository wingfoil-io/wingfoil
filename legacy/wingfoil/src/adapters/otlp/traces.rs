//! OTLP trace (span) export for `wingfoil` streams.
//!
//! The metrics side of the OTLP adapter (see `push.rs`) pushes scalar
//! gauges. This module does the other half: it turns `Traced<T, L>`-style
//! stream values into OpenTelemetry **spans** so high-cardinality
//! per-request data (session IDs, user IDs, trace IDs) can be routed to
//! a tracing backend like Grafana Tempo, Jaeger, or Honeycomb without
//! paying the Prometheus cardinality tax.
//!
//! For each tick of the upstream, the exporter emits a parent span
//! covering the full `stamps[0] .. stamps[N-1]` interval plus one child
//! span per adjacent stage pair. Attributes are supplied by a
//! user-provided closure so per-payload fields (`session.id`,
//! `client_seq`, …) ride along on the spans.
//!
//! # Example
//!
//! ```ignore
//! use wingfoil::adapters::otlp::{OtlpConfig, OtlpSpans};
//!
//! let config = OtlpConfig {
//!     endpoint: "http://localhost:4318".into(),
//!     service_name: "my-app".into(),
//! };
//! traced_stream.otlp_spans(config, "roundtrip", |p, attrs| {
//!     attrs.add("session.id", p.session_hex());
//!     attrs.add("client_seq", p.client_seq as i64);
//! });
//! ```
//!
//! Spans whose stamps are all zero (no stamping actually happened) or
//! whose endpoints go backwards are silently skipped — a partial
//! latency record does not corrupt the trace.

use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use opentelemetry::Context;
use opentelemetry::KeyValue;
use opentelemetry::trace::{
    Span as _, SpanKind, TraceContextExt as _, Tracer as _, TracerProvider as _,
};
use opentelemetry_otlp::{SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::trace::SdkTracerProvider;

use super::OtlpConfig;
use crate::RunMode;
use crate::latency::{HasLatency, Latency};
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;

/// Reusable buffer for attributes in `OtlpSpans`. Avoids allocating a new
/// `Vec<KeyValue>` on every tick by pre-allocating once and reusing.
pub struct OtlpAttributeBuffer {
    attrs: Vec<KeyValue>,
}

impl OtlpAttributeBuffer {
    /// Add a key-value attribute. The buffer is pre-allocated and reused
    /// across ticks, so this is efficient even at high tick rates.
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

/// Fluent sink method: export stream values as OTLP trace spans.
pub trait OtlpSpans<P>
where
    P: Element + HasLatency,
{
    /// Emit one parent span per upstream tick, plus one child span per
    /// adjacent stage pair. The parent's name is `span_name` (must be a
    /// static string literal); children are named `"<prev_stage>__<next_stage>"`.
    /// The attribute extractor is called once per tick. Use `buffer.add()` to
    /// populate attributes; the buffer is pre-allocated and reused.
    ///
    /// Spans with incomplete or backwards timestamps are silently skipped.
    /// Use this for high-cardinality per-request data (session IDs, user IDs)
    /// that would explode Prometheus label cardinality.
    #[must_use]
    fn otlp_spans<F>(
        self: &Rc<Self>,
        config: OtlpConfig,
        span_name: &'static str,
        attrs: F,
    ) -> Rc<dyn Node>
    where
        F: Fn(&P, &mut OtlpAttributeBuffer) + Send + Sync + 'static;
}

impl<P> OtlpSpans<P> for dyn Stream<P>
where
    P: Element + HasLatency + Send + 'static,
{
    fn otlp_spans<F>(
        self: &Rc<Self>,
        config: OtlpConfig,
        span_name: &'static str,
        attrs: F,
    ) -> Rc<dyn Node>
    where
        F: Fn(&P, &mut OtlpAttributeBuffer) + Send + Sync + 'static,
    {
        let attrs = std::sync::Arc::new(attrs);
        let consumer = Box::new(move |ctx: RunParams, source: Pin<Box<dyn FutStream<P>>>| {
            spans_consumer::<P, F>(config, span_name, attrs, ctx, source)
        });
        self.consume_async(consumer)
    }
}

async fn spans_consumer<P, F>(
    config: OtlpConfig,
    span_name: &'static str,
    attrs: std::sync::Arc<F>,
    ctx: RunParams,
    mut source: Pin<Box<dyn FutStream<P>>>,
) -> anyhow::Result<()>
where
    P: Element + HasLatency + Send,
    F: Fn(&P, &mut OtlpAttributeBuffer) + Send + Sync + 'static,
{
    // Historical / backtesting mode: drain and return. Matches the
    // metrics-push consumer's behaviour — no network traffic, same
    // graph wiring can run under both modes.
    if matches!(ctx.run_mode, RunMode::HistoricalFrom(_)) {
        while source.next().await.is_some() {}
        return Ok(());
    }

    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(&config.endpoint)
        .build()
        .map_err(|e| anyhow::anyhow!("otlp_spans: failed to build exporter: {e}"))?;

    let resource = opentelemetry_sdk::Resource::builder_empty()
        .with_service_name(config.service_name)
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("wingfoil");
    let stage_names = P::L::stage_names();
    let n = P::L::N;

    // Precompute hop names once to avoid allocation on every tick.
    let hop_names: Vec<String> = (1..n)
        .map(|i| format!("{}__{}", stage_names[i - 1], stage_names[i]))
        .collect();

    // Pre-allocate attribute buffer once and reuse across ticks.
    let mut attr_buffer = OtlpAttributeBuffer::default();

    while let Some((_time, value)) = source.next().await {
        emit_spans(
            &tracer,
            &value,
            attrs.as_ref(),
            span_name,
            n,
            &hop_names,
            &mut attr_buffer,
        );
    }

    drop(provider);
    Ok(())
}

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
    // A trace needs at least one real start/end pair. If neither endpoint
    // of the full journey has been stamped, skip.
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

    // One child span per hop. Skip hops whose endpoints were never
    // stamped so a partially-stamped record still produces the hops
    // that WERE stamped.
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
    use super::*;
    use crate::latency::Stage;
    use crate::nodes::*;
    use crate::{NanoTime, RunFor, Traced};
    use std::time::Duration as StdDuration;

    crate::latency_stages! {
        pub TestLatency {
            a,
            b,
            c,
        }
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    struct TestPayload {
        session: u64,
    }

    #[test]
    fn historical_mode_drains_without_connecting() {
        let config = OtlpConfig {
            endpoint: "http://127.0.0.1:1".into(),
            service_name: "test".into(),
        };
        let source: Rc<dyn Stream<Traced<TestPayload, TestLatency>>> =
            ticker(StdDuration::from_millis(10))
                .count()
                .map(|i: u64| Traced::<TestPayload, TestLatency>::new(TestPayload { session: i }));
        let node = source.otlp_spans(
            config,
            "test_span",
            |_p: &Traced<TestPayload, TestLatency>, attrs| {
                attrs.add("k", "v");
            },
        );
        node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
    }
}
