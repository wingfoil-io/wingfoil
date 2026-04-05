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
    ///
    /// Values are converted to `f64` via `T::to_string().parse::<f64>()`. Types
    /// whose `Display` output is not a plain number (e.g. `"42 units"`) will
    /// record `0.0` and emit a `log::warn`. Use a `.map()` upstream to extract
    /// a numeric field if your type does not format as a bare number.
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
    let gauge = meter.f64_gauge(metric_name).build();

    while let Some((_time, value)) = source.next().await {
        let s = value.to_string();
        let v: f64 = s.parse().unwrap_or_else(|_| {
            log::warn!("otlp_push: could not parse {s:?} as f64, recording 0.0");
            0.0
        });
        gauge.record(v, &[]);
    }

    provider
        .shutdown()
        .map_err(|e| anyhow::anyhow!("otlp_push: shutdown error: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NanoTime, RunFor, RunMode, nodes::*};
    use std::time::Duration;

    #[test]
    fn historical_mode_drains_without_connecting() {
        // No collector running — historical mode must return Ok without any network calls.
        let config = OtlpConfig {
            endpoint: "http://127.0.0.1:1".into(),
            service_name: "test".into(),
        };
        let counter = ticker(Duration::from_millis(10)).count();
        let node = counter.otlp_push("test_metric", config);
        node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
    }
}
