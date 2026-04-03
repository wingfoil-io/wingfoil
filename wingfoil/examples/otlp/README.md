# OTLP Adapter Example

Demonstrates pushing wingfoil stream metrics to any OpenTelemetry-compatible backend
(Grafana Alloy, Datadog, Honeycomb, New Relic, etc.) via OTLP HTTP, alongside a
Prometheus `/metrics` endpoint for pull-based scraping.

## Setup

```sh
docker run --rm -p 4318:4318 otel/opentelemetry-collector:latest
```

## Run

```sh
OTLP_ENDPOINT=http://localhost:4318 cargo run --example otlp --features otlp,prometheus
```

## Code

```rust
let exporter = PrometheusExporter::new("0.0.0.0:9091");
let port = exporter.serve()?;

let config = OtlpConfig {
    endpoint: "http://localhost:4318".into(),
    service_name: "wingfoil-example".into(),
};

let counter = ticker(Duration::from_secs(1)).count();
let metric_node = exporter.register("wingfoil_ticks_total", counter.clone());
let otlp_node = counter.otlp_push("wingfoil_ticks_total", config);

Graph::new(vec![metric_node, otlp_node], RunMode::RealTime, RunFor::Forever).run()?;
```

## Output

```
Prometheus metrics at http://localhost:9091/metrics
Pushing OTLP metrics to http://localhost:4318
```
