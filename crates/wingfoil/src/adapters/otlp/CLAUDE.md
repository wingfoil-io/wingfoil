# OTLP Adapter (wingfoil)

A realtime, **push-based** OpenTelemetry sink: stream values exported as OTLP
gauge metrics over HTTP/protobuf, plus a trace/span exporter for
latency-stamped payloads. Ports legacy `wingfoil::adapters::otlp` onto the Op
model.

**Sink only** — no source. It is the *push telemetry* half of
`/new-adapter-next` step 8 (the pull half is
[`prometheus`](../prometheus/CLAUDE.md)).

## Layout

```
adapters/
  otlp.rs          # OtlpConfig, OtlpSinkOps (metrics), OtlpAttributeBuffer + OtlpSpanOps (traces)
  otlp/CLAUDE.md   # this file
```

## Feature gating

```toml
otlp = ["dep:opentelemetry", "dep:opentelemetry_sdk", "dep:opentelemetry-otlp", "async"]
otlp-integration-test = ["otlp", "dep:testcontainers"]
```

**Version divergence from legacy is deliberate**: next pins opentelemetry
**0.32** where legacy is still on 0.28, rolled forward for GHSA-w9wp-h8wv-79jx
(register **D5**, won't-fix — legacy retires at cutover, so 0.32 is the
surviving version). This is the worked example of the `dependency-review`
gate's "roll forward rather than allowlist" rule in `/new-adapter-next` step 3.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `OtlpSinkOps::otlp_push(metric_name, config)` | sink trait on `Stream<T: Display>` | one gauge recording per tick |
| `OtlpSpanOps::otlp_spans(span_name, config, attrs)` | sink trait on `Stream<P: HasLatency>` | one parent span per tick + one child per stage hop |

`OtlpConfig::new(endpoint, service_name)`, with `From<(&str, &str)>` and
`From<(String, String)>`. Spans take an `OtlpAttributeBuffer` filled by a
caller closure — that is where high-cardinality per-request data (session IDs,
trace IDs) goes, routed to a tracing backend instead of paying the Prometheus
cardinality tax.

## What to know before changing it

- **Export runs off the graph thread.** The gauge recording *and* the whole
  OTel SDK export machinery (an HTTP/protobuf `MetricExporter` behind a 500 ms
  `rt-tokio` `PeriodicReader`) sit inside `consume_async`, so a slow or blocked
  collector never stalls the single-threaded engine.
- **The provider is built lazily, on the first exported value**, inside the
  consumer task's runtime context, and **dropped at teardown** — the drop is
  what flushes the final batch. `shutdown()` is *deliberately avoided*: its
  timeout can fail when the async runtime is unavailable
  (opentelemetry-rust #3137). Do not "improve" this into an explicit shutdown.
- **Historical replay is a no-op** — no value reaches the background task, so
  no meter provider is built and **no network calls happen at all**. Legacy's
  consumer checked the run mode and drained without connecting; next reads
  `Ctx::run_mode()` in the cycle. A backtest that includes the sink stays
  inert.
- **A non-numeric `Display` records `0.0` and logs a `log::warn`** (e.g.
  `"42 units"`). Legacy parity. Callers `.map()` upstream to extract a numeric
  field. Note this is *not* the same as the Python-binding rule about silent
  fallbacks — here it is legacy-preserved behaviour with a warning.
- `consume_async` ⇒ the `block_on` footgun (A5a): build, run and drop the graph
  from a **non-async** thread.
- **Argument order differs from legacy** for spans: next is
  `otlp_spans(span_name, config, attrs)`, legacy was
  `otlp_spans(config, span_name, attrs)`. Easy to get wrong when porting a
  call site.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `otlp.rs` — three
items: the graph-owned runtime, so `otlp_push` takes no `&Handle` (A5); the
sink-as-extension-trait fold (D1); and the span exporter's per-value
`consume_async` model with a lazily-built provider (legacy built its tracer
provider once up front inside its own consumer loop), plus the argument-order
change above.

Register **C1** ("otlp trace/span export not ported") is **resolved** —
`otlp_spans` landed with the Phase-5 latency infrastructure
(`Traced`/`HasLatency`/`latency_stages!`). Every span capability is present:
one parent span per tick, one child per stage hop, caller-supplied attributes,
and the silent skip of all-zero / backwards timestamps. Do not reintroduce a
"metrics only" note.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/otlp_adapter.rs` | `#![cfg(feature = "otlp")]` | nothing |
| `tests/otlp_integration.rs` | `#![cfg(feature = "otlp-integration-test")]` | an OTel collector (testcontainers) |

The integration test is **self-contained** — testcontainers starts the
collector, no manual Docker setup. To run the *example* against one:

```sh
docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0
```

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features otlp --test otlp_adapter
cargo test --manifest-path crates/wingfoil/Cargo.toml --features otlp-integration-test -- --test-threads=1
```

**Workflow:** `.github/workflows/otlp-next-integration.yml` (in
`integration-tests.yml`). Rust leg only — the Python tests are service-free.

## Example

`examples/otlp_adapter.rs`, `required-features = ["otlp", "prometheus"]` (it
demonstrates both telemetry sinks side by side).

## Python

`wingfoil-python` feature `otlp = ["wingfoil/otlp", "_common"]`.
**In `all-adapters` and in the wheel** (pure Rust — the OTel SDK over reqwest).

- Entry point: `otlp_push(stream, …)` only — **`otlp_spans` is not bound**
  (it needs a `HasLatency` payload type).
- The stream is taken **erased** and stringified via Python's `str()`, matching
  the Rust sink's `Display` contract.
- Binding this adapter changed the engine: `otlp_push` used to take a
  `&'static str` metric name, so the legacy binding did
  `Box::leak(name.into_boxed_str())` per wiring call. The SDK actually accepts
  `impl Into<Cow<'static, str>>`, so the trait bound was widened and the leak is
  gone; existing `&'static str` callers are unaffected.
- Tests: `tests/test_otlp.py`, **no marker** — runs by default in
  `next-python-test.yml`.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil/Cargo.toml --features otlp
```
