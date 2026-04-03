use std::pin::Pin;
use std::rc::Rc;

use futures::StreamExt;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_otlp::{MetricExporter, WithExportConfig as _};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

use crate::RunMode;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;

/// Connection configuration for an OTLP metrics endpoint.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    /// OTLP HTTP endpoint, e.g. `"http://localhost:4318"`.
    pub endpoint: String,
    /// Service name reported in OTLP resource attributes.
    pub service_name: String,
}

/// Fluent sink method to push stream values to an OTLP metrics endpoint.
pub trait OtlpPush<T: Element> {
    /// Push every tick of this stream as an OTLP gauge metric.
    #[must_use]
    fn otlp_push(self: &Rc<Self>, metric_name: &str, config: OtlpConfig) -> Rc<dyn Node>;
}

impl<T: Element + Send + std::fmt::Display + 'static> OtlpPush<T> for dyn Stream<T> {
    fn otlp_push(self: &Rc<Self>, metric_name: &str, config: OtlpConfig) -> Rc<dyn Node> {
        let metric_name = metric_name.to_string();
        let consumer = Box::new(move |ctx: RunParams, source: Pin<Box<dyn FutStream<T>>>| {
            push_consumer(metric_name, config, ctx, source)
        });
        self.consume_async(consumer)
    }
}

async fn push_consumer<T: Element + Send + std::fmt::Display>(
    metric_name: String,
    config: OtlpConfig,
    ctx: RunParams,
    mut source: Pin<Box<dyn FutStream<T>>>,
) -> anyhow::Result<()> {
    // Telemetry has no meaning in historical/backtesting mode — drain and return.
    if matches!(ctx.run_mode, RunMode::HistoricalFrom(_)) {
        while source.next().await.is_some() {}
        return Ok(());
    }

    // Build OTLP HTTP exporter + meter provider
    let exporter = MetricExporter::builder()
        .with_http()
        .with_endpoint(&config.endpoint)
        .build()
        .map_err(|e| anyhow::anyhow!("otlp_push: failed to build exporter: {e}"))?;

    let reader = PeriodicReader::builder(exporter)
        .with_interval(std::time::Duration::from_millis(500))
        .build();

    let resource = opentelemetry_sdk::Resource::builder_empty()
        .with_service_name(config.service_name)
        .build();

    let provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build();

    let meter = provider.meter("wingfoil");
    // f64_gauge requires Cow<'static, str>; leak the string to satisfy the bound.
    let metric_name_static: &'static str = Box::leak(metric_name.into_boxed_str());
    let gauge = meter.f64_gauge(metric_name_static).build();

    while let Some((_time, value)) = source.next().await {
        let v: f64 = value.to_string().parse().unwrap_or(0.0);
        gauge.record(v, &[]);
    }

    provider
        .shutdown()
        .map_err(|e| anyhow::anyhow!("otlp_push: shutdown error: {e}"))?;
    Ok(())
}
