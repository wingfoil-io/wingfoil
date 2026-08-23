# Prometheus Adapter Example (wingfoil)

Serve `GET /metrics` in the Prometheus text format so a scraper — or Grafana —
can read a wingfoil stream value. A port of the classic
`wingfoil/examples/telemetry/prometheus` example onto the wingfoil engine.

This is the **pull** half of the telemetry story; [`otlp`](../otlp/) is the push
half. The [`telemetry`](../telemetry/) directory carries a Docker harness that
stands up Prometheus and Grafana in front of both.

## Run

```sh
cargo run -p wingfoil --example prometheus_adapter --features prometheus
```

Then scrape it:

```sh
curl http://localhost:9091/metrics
```

Press Ctrl+C to stop — the graph runs `RunFor::Forever`.

## Code

```rust
// Bind the exporter synchronously so a bind error surfaces before the run.
let exporter = PrometheusExporter::new("0.0.0.0:9091");
let port = exporter.serve()?;

let g = GraphBuilder::new();

// The gauge sink is wired into `g` at this call; the returned handle is the
// graph's only root.
let _metric = g
    .ticker(Duration::from_secs(1))
    .count()
    .prometheus_gauge(&exporter, "wingfoil_ticks_total");

g.build().run(RunMode::RealTime, RunFor::Forever)?;
```

The exporter is bound **before** `build()`, deliberately: binding synchronously
means a port clash fails immediately with a clear error, rather than surfacing
somewhere inside the run.

`prometheus_gauge` is a sink — it attaches to the stream and becomes a graph
root. Every tick updates the gauge in place; the HTTP handler serves whatever the
current value is when a scrape arrives.

## Output

```text
Prometheus metrics available at http://localhost:9091/metrics
```

And from `curl`:

```text
# TYPE wingfoil_ticks_total gauge
wingfoil_ticks_total 5
```

## See also

- [`otlp`](../otlp/) — push to an OpenTelemetry backend instead of being scraped.
- [`telemetry`](../telemetry/) — the shared Prometheus + Grafana Docker harness.
- [`showcase/trading_e2e`](../../showcase/trading_e2e/) — both exporters in a
  full observability stack.
