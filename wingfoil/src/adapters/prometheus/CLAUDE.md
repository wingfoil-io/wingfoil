# Prometheus Adapter

Wingfoil adapter that serves a `GET /metrics` endpoint in Prometheus text format,
allowing Grafana (or any Prometheus-compatible system) to scrape stream values.

## Module Structure

```
prometheus/
  mod.rs               # Public API re-exports, module-level docs
  exporter.rs          # PrometheusExporter — hand-rolled HTTP server + MutableNode sink
  integration_tests.rs # Integration tests (requires running Prometheus; see below)
  CLAUDE.md            # This file
```

## Key Design Decisions

- Prometheus text format is hand-rolled (no `prometheus` crate) — format is simple, avoids a heavy dep.
- `PrometheusExporter` spawns its own OS thread (same pattern as the ZMQ publisher).
  `serve()` binds synchronously so bind errors surface before the graph starts.
- Metric nodes are regular `MutableNode` sinks; they read `peek_value()` each tick and write to a
  shared `Arc<Mutex<HashMap>>` that the HTTP thread reads on each scrape.
- **Historical / backtesting mode**: `setup()` detects `RunMode::HistoricalFrom` and sets an internal
  flag so `cycle()` becomes a no-op. No metrics are written and no connections are made. The HTTP
  server is still started if `serve()` was called, but it just serves an empty body.
- `reqwest` blocking client used in integration tests to keep them in plain `#[test]` functions.
- The adapter is pull-based: the exporter does not push anywhere — Prometheus scrapes it.

## Feature Flags

- `prometheus` — enables the adapter (pulls in `reqwest` with the `blocking` feature).
- `prometheus-integration-test` — enables `prometheus` + integration tests that require a running
  Prometheus instance (see below).

## Pre-Commit Requirements

```bash
# 1. Standard checks
cargo fmt --all
cargo clippy --workspace --all-targets --all-features

# 2. Unit tests (no external dependencies)
cargo test --features prometheus -p wingfoil -- adapters::prometheus

# 3. Integration tests (requires Docker stack)
docker compose -f docker/grafana/docker-compose.yml up -d
RUST_LOG=INFO cargo test --features prometheus-integration-test -p wingfoil \
  -- --test-threads=1 --nocapture adapters::prometheus::integration_tests
docker compose -f docker/grafana/docker-compose.yml down
```

## Integration Test Environment Variables

| Variable                | Default                 | Notes                              |
|-------------------------|-------------------------|------------------------------------|
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090` | URL of the running Prometheus      |
| `WINGFOIL_METRICS_PORT` | `9091`                  | Port the exporter binds on         |

## Gotchas

- The Prometheus scrape interval in `docker/grafana/provisioning/prometheus/prometheus.yml` is 5 s.
  The integration test polls for up to 60 s to account for scrape lag.
- Port conflicts: unit tests use fixed ports (19091, 19092). If those are already bound the tests
  will fail with "address already in use". Use `0` for OS-assigned ports in new tests.
