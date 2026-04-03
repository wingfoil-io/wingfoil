# Grafana Metrics Example

Demonstrates both Grafana integration points:

1. **Prometheus exporter** — serves `GET /metrics` on port 9091 so Grafana can scrape it via its Prometheus data source.
2. **Grafana Live push** — streams values directly to a Grafana Live channel for zero-latency panel updates.

## Setup

Start the Docker stack from the repo root:

```sh
cd docker/grafana && docker compose up -d   # start in background
docker compose ps                           # check all services are healthy
export GRAFANA_API_KEY=$(cat tokens/grafana_api_key)  # key is auto-created
# when done:
# docker compose down
```

This starts:
- **Grafana** on <http://localhost:3000> (admin / admin)
- **Prometheus** on <http://localhost:9090>, pre-configured to scrape `host.docker.internal:9091`

## Run

Prometheus exporter only (no API key needed):

```sh
RUST_LOG=info cargo run --example grafana_metrics --features grafana
```

Both exporter and Grafana Live push:

```sh
RUST_LOG=info GRAFANA_API_KEY=$(cat docker/grafana/tokens/grafana_api_key) cargo run --example grafana_metrics --features grafana
```

## What it does

```
ticker(1s) → count() ──┬── PrometheusExporter  →  GET /metrics  →  Prometheus  →  Grafana dashboard
                       └── grafana_push()       →  Grafana Live channel  →  Grafana streaming panel
```

The counter increments by 1 each second. After a few seconds you should see `wingfoil_ticks_total`
appear in the Prometheus UI at <http://localhost:9090> and in any Grafana panel pointed at the
`wingfoil_ticks_total` metric or the `stream/wingfoil/ticks` Live channel.

## Output

```
Prometheus metrics available at http://localhost:9091/metrics
Pushing to Grafana Live: stream/wingfoil/ticks
```

Scraping `http://localhost:9091/metrics` returns:

```
# TYPE wingfoil_ticks_total gauge
wingfoil_ticks_total 5
```
