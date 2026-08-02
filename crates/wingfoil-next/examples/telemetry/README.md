# Telemetry Examples (wingfoil-next)

Wingfoil provides two adapters for exporting metrics. Both work with Grafana as a
visualisation layer — they differ only in how data is transported.

A port of the legacy `legacy/wingfoil/examples/telemetry` guide onto the next engine.

## Which should I use?

| | `prometheus` | `otlp` |
|---|---|---|
| **Model** | Pull — Prometheus scrapes your app | Push — your app pushes to a collector |
| **Best for** | Existing Prometheus/Grafana stacks | Cloud-native / multi-vendor stacks |
| **Backends** | Prometheus, Mimir, Thanos | Grafana Alloy, Datadog, Honeycomb, New Relic, … |
| **Setup** | Zero config — just expose a port | Requires an OTel collector |
| **Standard** | De facto today | Emerging standard (growing fast) |

When in doubt, start with `prometheus` — it works with everything and needs no extra infrastructure.
Use `otlp` if you're pushing to a cloud backend or already running an OTel collector.

## Historical / backtesting mode

Both adapters are **silent no-ops** in historical mode (`RunMode::HistoricalFrom`).
The stream is consumed and discarded without connecting to any external service.
This means you can include telemetry in a strategy graph and run backtests freely —
no metrics will be emitted and no connections will be attempted.

## Examples

The two example programs live at the top of `examples/`, alongside the other
per-adapter examples:

| Example | Adapter | Source |
|---|---|---|
| `prometheus_adapter` | `PrometheusExporter` + `prometheus_gauge` | [`../prometheus_adapter.rs`](../prometheus_adapter.rs) |
| `otlp_adapter` | the above plus `otlp_push` | [`../otlp_adapter.rs`](../otlp_adapter.rs) |

## Running

This directory carries the Grafana + Prometheus stack and a wrapper that brings
it up and launches either example against it:

```sh
./run.sh              # prometheus (pull) — the default
./run.sh otlp         # prometheus + OTLP push
```

- **Grafana** on <http://localhost:3000> (anonymous admin, no login)
- **Prometheus** on <http://localhost:9090>, scraping the example on port 9091

Press `Ctrl+C` to stop; the stack is torn down on exit.

To run an example without the stack — the exporter is a server in its own right,
so `curl` is enough to see it working:

```sh
cargo run -p wingfoil-next --example prometheus_adapter --features prometheus &
curl http://localhost:9091/metrics
# # TYPE wingfoil_ticks_total gauge
# wingfoil_ticks_total 3
```

The OTLP half needs a collector listening on 4318:

```sh
docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0
OTLP_ENDPOINT=http://localhost:4318 \
    cargo run -p wingfoil-next --example otlp_adapter --features otlp,prometheus
```

## Deviations from legacy

- **One `run.sh` instead of two.** Legacy ships
  `telemetry/prometheus/run.sh` and `telemetry/otlp/run.sh`, which differ only
  in the example they launch; this takes the example name as an argument.
- **The docker stack lives with the example.** Legacy's compose file sits
  under the adapter source tree (`legacy/wingfoil/src/adapters/prometheus/docker/`)
  because its integration tests share it; next's adapter tests need no stack,
  so it belongs here.
- **No `grafana-init` service.** That container exists to mint a Grafana API
  token for the legacy adapter's integration tests. No example reads it, and
  legacy's `otlp/run.sh` blocked for up to 30s waiting on a token it never
  used.
