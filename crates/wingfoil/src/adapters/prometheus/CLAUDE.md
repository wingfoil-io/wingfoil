# Prometheus Adapter (wingfoil)

A realtime, **pull-based** metrics sink: a hand-rolled `GET /metrics` endpoint
in Prometheus text format that Grafana (or any Prometheus-compatible system)
can scrape. Ports legacy `wingfoil::adapters::prometheus` onto the Op model.

**Sink only** — there is no source and no `_read`/`_sub`. It is the reference
for the *pull-based exporter* shape in `/new-adapter` (step 8).

## Layout

```
adapters/
  prometheus.rs          # PrometheusExporter (registry + HTTP thread), PrometheusSinkOps
  prometheus/CLAUDE.md   # this file
```

## Feature gating

```toml
prometheus = ["dep:arc-swap"]
prometheus-integration-test = ["prometheus", "dep:reqwest"]
```

`arc-swap` is the **only** runtime dependency — the HTTP server is hand-rolled
on `std::net`, with no Prometheus client crate. Keep it that way.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `PrometheusExporter::new(addr)` | handle | owns the metric registry |
| `PrometheusExporter::serve() -> Result<u16>` | handle | binds **synchronously**, spawns the HTTP thread, returns the port |
| `PrometheusSinkOps::prometheus_gauge(&exporter, name)` | sink trait on any `Stream<T: Display>` | returns the sink `Stream<()>` |

## What to know before changing it

- **The metric-slot model is the whole design.** Each registered stream gets
  its own `Arc<ArcSwapOption<String>>`. Every realtime cycle the sink
  stringifies the current value and publishes it with a **single atomic pointer
  swap** — never a lock on the graph thread. The scrape thread snapshots the
  registry (locked only at registration and once per scrape, off the graph
  thread) and loads each slot to render. This is the `ArcSwap`
  graph-publishes / background-thread-reads hand-off the invariants section of
  `/new-adapter` describes; do not replace it with a `Mutex`.
- **A slot never written is omitted** from the response (`None`), not rendered
  as zero.
- **Historical replay is a no-op.** Under `RunMode::HistoricalFrom` the sink
  writes no slot, so a backtest never publishes fast-forwarded values to a live
  endpoint. The server, if `serve()` was called, still answers — with an empty
  body. Legacy detected the run mode in `setup` and short-circuited `cycle`;
  wingfoil reads `Ctx::run_mode()` in the cycle itself, so the same wiring runs
  deterministically in both modes.
- **`serve()` binds synchronously**, so a bind error surfaces before the run —
  legacy parity, deliberately *not* deferred to `start()`.
- Anything other than `/metrics` gets a 404 (legacy parity).

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `prometheus.rs` — two
items: (1) the sink is an **extension trait**, `stream.prometheus_gauge(&exporter,
name)`, rather than legacy's `exporter.register(name, stream)` — the exporter
still owns the registry (register D1); (2) `serve` returns `anyhow::Result`
instead of `Result<u16, std::io::Error>`, per the fallible-with-context
convention (register **D2**). Every legacy capability is preserved: the
text endpoint, per-metric slots omitted until first written, the historical
no-op, the 404, and the synchronous bind.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/prometheus_adapter.rs` | `#![cfg(feature = "prometheus")]` | nothing |
| `tests/prometheus_integration.rs` | `#![cfg(feature = "prometheus-integration-test")]` | the Docker stack |

`prometheus_adapter.rs` ports legacy's exporter unit tests **plus** its
self-contained `multiple_metrics` integration test — because the adapter *is*
the server, a bind-port-0 → run → scrape-over-loopback round trip needs no
service and belongs in the default tier.

`prometheus_integration.rs` is the one that genuinely needs a live Prometheus
scraping the exporter, and reuses the **telemetry example's** compose stack —
the same Prometheus config, scraping port 9091 on the host:

```sh
docker compose -f crates/wingfoil/examples/adapters/telemetry/docker/docker-compose.yml up -d
```

It reads Prometheus only, so the stack's Grafana comes up unused and the
legacy stack's `grafana-init` token minting has no counterpart here.

```bash
cargo test -p wingfoil --features prometheus --test prometheus_adapter
cargo test -p wingfoil --features prometheus-integration-test -- --test-threads=1
```

**Workflow:** `.github/workflows/prometheus-integration.yml` (in
`integration-tests.yml`). Rust leg only — the Python tests are service-free.

## Example

`examples/prometheus_adapter.rs`, `required-features = ["prometheus"]`. The
otlp example also requires `prometheus`
(`required-features = ["otlp", "prometheus"]`).

## Python

`wingfoil-python` feature
`prometheus = ["wingfoil/prometheus", "_common"]`. **In `all-adapters` and
in the wheel** (pure Rust).

- **Hand-written, not `#[pyadapter]`**: the exporter is a stateful handle with
  a lifecycle, which the macro has no shape for. `src/adapters/prometheus.rs`
  exposes a `#[pyclass] PrometheusExporter` with `serve()` and
  `gauge(name, stream)`, registered via `m.add_class::<…>()` under the same
  `#[cfg]` as the functions. It is the **minimal** example of that pattern —
  note it takes no `Graph` at all, because the exporter owns no graph state and
  the stream it is handed carries its own.
- Tests: `tests/test_prometheus.py`, **no marker** — the scrape-after-run round
  trip needs no live wall clock, so it runs by default in
  `python-test.yml`.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil --features prometheus
```
