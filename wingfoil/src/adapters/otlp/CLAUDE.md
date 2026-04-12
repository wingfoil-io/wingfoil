# OTLP Adapter

Wingfoil adapter that pushes stream values as OpenTelemetry gauge metrics to any
OTLP-compatible backend (Grafana Alloy, Datadog, Honeycomb, New Relic, etc.).

## Module Structure

```
otlp/
  mod.rs               # Public API re-exports, module-level docs
  push.rs              # OtlpPush trait + push_consumer async fn
  integration_tests.rs # Integration tests (testcontainers — no external setup needed)
  CLAUDE.md            # This file
```

## Key Design Decisions

- Uses the OpenTelemetry Rust SDK (`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`).
  Metrics are exported via HTTP/protobuf (`http-proto` feature) to the OTLP `/v1/metrics` endpoint.
- A `SdkMeterProvider` with a 500 ms `PeriodicReader` is created per consumer invocation so that
  the final batch of metrics is flushed on `shutdown()` before the function returns.
- The metric name is leaked to a `&'static str` (`Box::leak`) because the OTel SDK's
  `f64_gauge` builder requires `Cow<'static, str>`. This is a one-time allocation per run.
- **Historical / backtesting mode**: the consumer checks `ctx.run_mode` via `RunParams` and
  drains the source stream without connecting to any external service. No OTel provider is
  built and no network calls are made.
- The adapter is push-based (contrast with `prometheus` which is pull-based).
- `OtlpPush` is implemented for `dyn Stream<T>` where `T: Display` so any numeric or string
  stream can be pushed without wrapping.

## Feature Flags

- `otlp` — enables the adapter (pulls in `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
  `reqwest`).
- `otlp-integration-test` — enables `otlp` + `testcontainers` for self-contained integration tests.

## Pre-Commit Requirements

```bash
# 1. Standard checks
cargo fmt --all
cargo clippy --workspace --all-targets --all-features

# 2. Unit / doc tests (no external dependencies)
cargo test --features otlp -p wingfoil -- adapters::otlp

# 3. Integration tests (testcontainers pulls the collector image automatically)
RUST_LOG=INFO cargo test --features otlp-integration-test -p wingfoil \
  -- --test-threads=1 --nocapture adapters::otlp::integration_tests
```

## Integration Tests

Tests use `testcontainers` with `SyncRunner` to start an `otel/opentelemetry-collector` container
automatically — no manual Docker setup required. The container is stopped when dropped.

## Gotchas

- The OTel SDK logs its own startup/shutdown messages to stderr even with `RUST_LOG` unset.
  This is expected and harmless in tests.
- `PeriodicReader` interval is set to 500 ms to ensure at least one export happens during
  short-running tests. Decrease if tests become flaky; increase if the backend is slow.
- Non-numeric stream values are parsed as f64 via `.to_string().parse()`. Values that fail
  parsing are recorded as `0.0` and a warning is logged.
- The collector image version is pinned (`0.116.0`) to avoid unexpected API changes.
  Update the pin deliberately and re-run integration tests.
