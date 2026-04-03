# Prometheus Metrics Example

Demonstrates both Grafana integration points:

1. **Prometheus exporter** — serves `GET /metrics` on port 9091 so Grafana can scrape it via its Prometheus data source.
2. **Grafana Live push** — streams values directly to a Grafana Live channel for zero-latency panel updates.

## Run

```sh
./wingfoil/examples/prometheus/run.sh
```

This starts the Docker stack (Grafana + Prometheus), waits for the API key to be provisioned, then runs the example. Press `Ctrl+C` to stop the example; the Docker stack keeps running.

- **Grafana** on <http://localhost:3000> (no login required)
- **Prometheus** on <http://localhost:9090>

To stop the stack:
```sh
docker compose -f docker/grafana/docker-compose.yml down
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
