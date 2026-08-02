# kafka Adapter (wingfoil-next)

A streaming topic-consume **source** and a topic-produce **sink** for Apache
Kafka, over `rdkafka`. Ports legacy `wingfoil::adapters::kafka` onto the Op
model.

## Layout

```
adapters/
  kafka.rs          # connection/record/event types, kafka_sub, kafka_source, KafkaSinkOps
  kafka/CLAUDE.md   # this file
```

## Feature gating

```toml
kafka = ["dep:rdkafka", "dep:async-stream", "async"]
kafka-integration-test = ["kafka", "dep:testcontainers"]
```

`rdkafka` builds the bundled `librdkafka` via its default path
(`./configure` + `make`), **not** `cmake-build` — same as legacy; the cmake
path's `curl.h` dependency breaks CI.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `kafka_sub(g, run_mode, conn, topic, group_id)` | source | live consumer, `Result<Stream<Burst<KafkaEvent>>>` |
| `kafka_source(g, params, conn, topic, cfg)` | source | mode-agnostic dispatcher over `KafkaSourceConfig` |
| `KafkaSinkOps::kafka_pub(conn)` | sink trait | on `Stream<Burst<KafkaRecord>>` **and** `Stream<KafkaRecord>` |

Types: `KafkaConnection` (+ `From<&str>`/`String`/`&String`), `KafkaRecord`
(target topic, key, value — so one sink can write many topics), `KafkaEvent`.

## What to know before changing it

- **`kafka_sub` is realtime-only, rejected at wiring** (register B2, ratified).
  Its `recv()` loop never ends, so the historical channel path would deadlock
  at `start`. Legacy technically *permitted* a `HistoricalFrom` run with
  wall-clock timestamps; next errors clearly instead. `run_mode` exists only
  for that check.
- **Prefer `kafka_source` at new call sites.** It takes `RunParams` and
  dispatches on the run's `RunMode`, so the mode choice stays at `run()`
  instead of in the function name. Only the **live half exists today** — a
  bounded offset-range replay reader is not implemented, so a historical run
  errors at wiring naming the missing half (register B2's "agreed plan"). When
  that reader lands, it goes behind `KafkaSourceConfig`, and `kafka_source`
  call sites need no change.
- **Consumer-group semantics are the caller's lever.** Same `group_id` across
  runs continues from the last committed offset; a fresh group reads from the
  beginning (`auto.offset.reset = earliest`). Delivery is **at-most-once** —
  `enable.auto.commit = true` commits in the background. For at-least-once,
  callers disable auto-commit and manage offsets through rdkafka directly;
  don't bake that into the adapter.
- **The sink produces a whole burst concurrently** — it rides
  `consume_async_bursts` and drains the burst's delivery futures together via
  `FuturesUnordered`, so per-burst latency is ~one broker roundtrip rather
  than N (register **B3**; `consume_async_bursts` was added for exactly this).
  Order is preserved *across* bursts. Do not regress it to a sequential
  `consume_async`.
- **No explicit `producer.flush()`.** Every send is awaited to its delivery
  ack, so nothing is left queued, and the consumer drains all queued bursts at
  teardown. Legacy's end-of-stream flush is unnecessary here.
- **The `FutureProducer` is created at wiring but connects lazily.**
  `ClientConfig::create()` is a config check with no socket, so the factory
  returns `Result` for a bad config while a bad *broker* surfaces as a produce
  failure during the run — already in line with defer-to-start (register A1),
  no migration needed.
- `consume_async_bursts` means the usual `block_on` footgun (A5a): build, run
  and drop the graph from a **non-async** thread.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `kafka.rs` — the
graph-owned runtime (A5), the wiring-time historical rejection (B2), and the
sink-as-trait fold (D1). Every legacy capability (consumer-group offset
tracking, earliest auto-offset-reset, per-record topic/key/partition,
at-most-once via background auto-commit, multi-topic writes from one sink) is
preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/kafka_adapter.rs` | `#![cfg(feature = "kafka")]` | nothing |
| `tests/kafka_integration.rs` | `#![cfg(feature = "kafka-integration-test")]` | a broker container |

```bash
cargo test -p wingfoil-next --features kafka --test kafka_adapter
cargo test -p wingfoil-next --features kafka-integration-test -- --test-threads=1
```

Local broker (Redpanda — Kafka-compatible, faster startup):

```sh
docker run --rm -p 9092:9092 \
  docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
  redpanda start --overprovisioned --smp 1 --memory 512M \
  --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
```

**Workflow:** `.github/workflows/kafka-next-integration.yml` (in
`integration-tests.yml`), Rust leg + `pytest -m requires_kafka` Python leg.
Note `kafka-python-integration.yml` is the **legacy** Python binding's.

## Example

`examples/kafka_adapter.rs`, `required-features = ["kafka"]`.

## Python

`wingfoil-next-python` feature `kafka = ["wingfoil-next/kafka", "_common"]`.
**In `all-adapters` and in the wheel** — librdkafka is built from source, which
needs only a C toolchain (present in every CI image), so it is not a
portability exclusion.

- Entry points: `kafka_sub(graph, …)`, `kafka_pub(stream, …)` — `#[pyadapter]`,
  in `src/adapters/kafka.rs`. `kafka_source` is **not** bound (its historical
  half does not exist yet).
- Tests: `tests/test_kafka.py` — service-free group by default,
  `@pytest.mark.requires_kafka` group in the workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features kafka
# with a broker available:
cargo test -p wingfoil-next --features kafka-integration-test -- --test-threads=1
```
