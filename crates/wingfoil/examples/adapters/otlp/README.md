# OTLP Adapter Example (wingfoil)

Push a wingfoil stream value to an OpenTelemetry backend over OTLP, **alongside**
a Prometheus `/metrics` endpoint for pull-based scraping. A port of the classic
`wingfoil/examples/telemetry/otlp` example onto the wingfoil engine.

Both exporters run over the same counter, so the metric is available to
Prometheus scrapers *and* to any OTLP-compatible backend — Grafana Alloy,
Datadog, Honeycomb, New Relic, and so on. That pairing is the point: you rarely
get to pick just one.

## Setup

An OTLP collector to receive the pushes:

```sh
docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0
```

## Run

```sh
OTLP_ENDPOINT=http://localhost:4318 \
    cargo run --manifest-path crates/wingfoil/Cargo.toml --example otlp_adapter --features otlp,prometheus
```

Press Ctrl+C to stop — the graph runs `RunFor::Forever`.

## Code

The counter feeds two sinks: a Prometheus gauge (pull) and an OTLP push export.

```rust
// ── Prometheus exporter (pull) ──
let exporter = PrometheusExporter::new("0.0.0.0:9091");
let port = exporter.serve()?;

// ── OTLP push ──
let endpoint = std::env::var("OTLP_ENDPOINT")
    .unwrap_or_else(|_| "http://localhost:4318".into());
let config = OtlpConfig::new(endpoint.clone(), "wingfoil-example");

let g = GraphBuilder::new();
let counter = g.ticker(Duration::from_secs(1)).count();
let _prometheus = counter.prometheus_gauge(&exporter, "wingfoil_ticks_total");
let _otlp       = counter.otlp_push("wingfoil_ticks_total", config)?;

g.build().run(RunMode::RealTime, RunFor::Forever)?;
```

`counter` is read by both sinks — a shared node, executed once per cycle with the
tick fanned out, not once per sink.

`OtlpConfig` carries the endpoint and the **service name** (`wingfoil-example`
here), which is what the backend groups the metrics under. `OTLP_ENDPOINT`
overrides the default at run time.

The graph owns the tokio runtime `otlp_push` exports on, created lazily, so no
`&Handle` is threaded in. Because `consume_async` requires it, the graph is
driven from a **non-async** `main`.

## Output

```text
Prometheus metrics at http://localhost:9091/metrics
Pushing OTLP metrics to http://localhost:4318
```

The collector's own log shows the received metrics; `curl
http://localhost:9091/metrics` shows the same value on the pull side.

## See also

- [`prometheus`](../prometheus/) — the pull half on its own.
- [`telemetry`](../telemetry/) — the shared Docker harness (Prometheus, Grafana, Alloy).
- [`showcase/trading_e2e`](../../showcase/trading_e2e/) — both in a full stack,
  with Tempo for traces.
