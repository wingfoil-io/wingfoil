# Prometheus Adapter

Wingfoil adapter for Prometheus metrics export, gated behind the `prometheus` feature flag.

## Module Structure

```
prometheus/
  mod.rs                # Public API re-exports
  prometheus.rs         # PrometheusExporter — serves GET /metrics
  live.rs               # grafana_push sink — POSTs to Grafana Live
  integration_tests.rs  # Integration tests (requires Docker stack)
```

## Pre-Commit Requirements

Before committing changes to this adapter, you MUST:

1. **Start the Docker stack:**
   ```bash
   docker compose -f docker/grafana/docker-compose.yml up -d  # start in background; docker compose -f docker/grafana/docker-compose.yml down when done
   ```

2. **Run integration tests (API key is read automatically from tokens/grafana_api_key):**
   ```bash
   RUST_LOG=INFO cargo test --features prometheus-integration-test -p wingfoil -- --test-threads=1 --nocapture
   ```
   See `docker/grafana/README.md` for how to obtain the API key.

3. **Run standard checks:**
   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features
   cargo test -p wingfoil
   ```

## Feature Flags

- `prometheus` — enables the adapter (adds `reqwest` dep)
- `prometheus-integration-test` — enables `prometheus` + integration tests that require the Docker stack

## Environment Variables (integration tests)

| Variable                | Default                 |
|-------------------------|-------------------------|
| `GRAFANA_TEST_URL`      | `http://localhost:3000` |
| `GRAFANA_TEST_API_KEY`  | required                |
| `GRAFANA_TEST_ORG_ID`   | `1`                     |
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090` |
| `WINGFOIL_METRICS_PORT` | `9091`                  |

## Key Design Decisions

- Prometheus text format is hand-rolled (no `prometheus` crate) — format is simple, avoids a dep
- `reqwest` with `blocking` feature is used for Grafana Live push to stay off the async executor
- `PrometheusExporter` spawns its own thread (same pattern as ZMQ publisher)
- `grafana_push` is a sink returning `Rc<dyn Node>` — same pattern as `kdb_write` and `zmq_pub`
- Grafana Live payload: Influx line protocol via `/api/live/push/:streamId` (single segment, no slashes) — JSON frames were tried but Grafana 11 parses the push endpoint as `labels_column` Influx format; numeric values formatted as float (`42.0`), strings quoted; slashes in stream_id auto-replaced with `_`
