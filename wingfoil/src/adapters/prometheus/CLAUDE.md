# Prometheus Adapter

Wingfoil adapter that serves a `GET /metrics` endpoint in Prometheus text format,
allowing Grafana (or any Prometheus-compatible system) to scrape stream values.

## Module Structure

```
prometheus/
  mod.rs               # Public API re-exports, module-level docs
  exporter.rs          # PrometheusExporter — hand-rolled HTTP server + MutableNode sink
  integration_tests.rs # Integration tests (requires running Prometheus; see below)
  docker/              # Docker Compose stack (Prometheus + Grafana) for integration tests
  CLAUDE.md            # This file
```

## Key Design Decisions

- Prometheus text format is hand-rolled (no `prometheus` crate) — format is simple, avoids a heavy dep.
- `PrometheusExporter` spawns its own OS thread (same pattern as the ZMQ publisher).
  `serve()` binds synchronously so bind errors surface before the graph starts.
- Metric nodes are regular `MutableNode` sinks; they read `peek_value()` each tick and publish the
  stringified value into a per-metric `Arc<ArcSwap<String>>` slot with a lock-free atomic pointer
  swap. The exporter holds a `Mutex<Vec<(name, slot)>>` registry that is locked only at
  `register()` time and once per HTTP scrape (off the graph thread) — never from `cycle()`. On
  scrape the HTTP thread snapshots the registry, drops the lock, then `.load()`s each slot to
  render the response.
- **Historical / backtesting mode**: `setup()` detects `RunMode::HistoricalFrom` and sets an internal
  flag so `cycle()` becomes a no-op. No metrics are written and no connections are made. The HTTP
  server is still started if `serve()` was called, but it just serves an empty body.
- `reqwest` blocking client used in integration tests to keep them in plain `#[test]` functions.
- The adapter is pull-based: the exporter does not push anywhere — Prometheus scrapes it.

## Feature Flags

- `prometheus` — enables the adapter (no external client crate; HTTP server is hand-rolled with `std::net`).
- `prometheus-integration-test` — enables `prometheus` + `reqwest` (blocking) for the integration tests.

## Pre-Commit Requirements

```bash
# 1. Standard checks
cargo fmt --all
cargo clippy --workspace --all-targets --all-features

# 2. Unit tests (no external dependencies)
cargo test --features prometheus -p wingfoil -- adapters::prometheus

# 3. Integration tests (requires Docker stack)
docker compose -f wingfoil/src/adapters/prometheus/docker/docker-compose.yml up -d
RUST_LOG=INFO cargo test --features prometheus-integration-test -p wingfoil \
  -- --test-threads=1 --nocapture adapters::prometheus::integration_tests
docker compose -f wingfoil/src/adapters/prometheus/docker/docker-compose.yml down
```

## Integration Test Environment Variables

| Variable                | Default                 | Notes                              |
|-------------------------|-------------------------|------------------------------------|
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090` | URL of the running Prometheus      |
| `WINGFOIL_METRICS_PORT` | `9091`                  | Port the exporter binds on         |

## Gotchas

- The Prometheus scrape interval in `docker/provisioning/prometheus/prometheus.yml` is 5 s.
  The integration test polls for up to 60 s to account for scrape lag.
- All tests (unit and integration) use port `0` so the OS assigns a free port. Never hardcode a port
  number in tests.
