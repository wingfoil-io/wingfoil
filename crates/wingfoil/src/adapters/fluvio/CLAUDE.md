# fluvio Adapter (wingfoil)

A streaming topic-partition consume **source** and a topic-produce **sink** for
[Fluvio](https://fluvio.io) clusters. Ports legacy
`wingfoil::adapters::fluvio` onto the Op model. Structurally the
[`kafka`](../kafka/CLAUDE.md) port's twin — read that one alongside this.

## Layout

```
adapters/
  fluvio.rs          # connection/record/event types, fluvio_sub, fluvio_source, FluvioSinkOps
  fluvio/CLAUDE.md   # this file
```

## Feature gating

```toml
fluvio = ["dep:fluvio", "dep:async-stream", "async"]
fluvio-integration-test = ["fluvio", "dep:testcontainers", "dep:fluvio-controlplane-metadata"]
```

`fluvio-controlplane-metadata` is used only by the integration tests, to
register the SPU with the SC before starting it.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `fluvio_sub(g, run_mode, conn, topic, partition, start_offset)` | source | `Result<Stream<Burst<FluvioEvent>>>` |
| `fluvio_source(g, params, conn, topic, partition, cfg)` | source | mode-agnostic dispatcher over `FluvioSourceConfig` |
| `FluvioSinkOps::fluvio_pub(conn, topic, buffer_size)` | sink trait | on `Stream<Burst<FluvioRecord>>` **and** `Stream<FluvioRecord>` |

Types: `FluvioConnection` (+ `From<&str>`/`String`/`&String`), `FluvioRecord`
(`new` / `with_key`), `FluvioEvent` (carries `offset`).

## What to know before changing it

- **No snapshot/watch duality.** Fluvio is a pure log, so `start_offset` alone
  chooses between "replay everything retained" and "tail from here":
  `None` → `Offset::beginning()`, `Some(n)` → absolute offset `n`, inclusive.
- **No consumer-group offset tracking in this adapter.** Each run starts where
  `start_offset` says. A resumable consumer records the last
  `FluvioEvent::offset` it saw and passes `Some(offset + 1)` next run. Don't
  invent hidden offset persistence.
- **`fluvio_sub` is realtime-only, rejected at wiring** (register B2). It
  replays retained records and then blocks forever, so the historical channel
  path would deadlock at `start`. Legacy technically permitted a
  `HistoricalFrom` run with wall-clock timestamps.
- **`fluvio_source` exists for the same reason as `kafka_source`** — mode
  dispatch at `run()` instead of in the function name — and likewise has only
  the **live half** today; a historical run errors at wiring naming the missing
  bounded offset-range reader.
- **A negative `start_offset` is rejected at wiring.** Legacy deferred that
  into the producer future so it surfaced at run start; the check is pure, so
  next fails fast.
- **The sink batches per burst**: one `send()` per record, then a single
  `flush()` per burst — throughput within a tick, delivery guaranteed before
  the run moves on (legacy parity). `key: None` sends with `RecordKey::NULL`.
- **The sink connects lazily on its first burst** (register A1/A4), so an
  unreachable cluster aborts the *run*, not graph construction.
- `consume_async_bursts` ⇒ the `block_on` footgun (A5a): build, run and drop
  the graph from a **non-async** thread.
- **Topics must exist before connecting** — Fluvio returns `TopicNotFound`
  otherwise. That is why there is a `fluvio_admin` example.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `fluvio.rs` — five
items: graph-owned runtime (A5), wiring-time historical rejection (B2),
sink-as-trait fold (D1), lazy sink connect (A1/A4), and the wiring-time
negative-offset rejection. Every legacy capability (offset-selected partition
consumption, keyed and keyless records, per-burst flush batching, the
single-record convenience sink) is preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/fluvio_adapter.rs` | `#![cfg(feature = "fluvio")]` | nothing |
| `tests/fluvio_integration.rs` | `#![cfg(feature = "fluvio-integration-test")]` | an SC + SPU |

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fluvio --test fluvio_adapter
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fluvio-integration-test -- --test-threads=1
```

Local cluster — Fluvio is **not a single process**: it needs an SC (System
Controller, port 9003) and at least one SPU (Stream Processing Unit, port
9010):

```sh
fluvio cluster start --local
fluvio topic create my-topic
```

**Workflow:** `.github/workflows/fluvio-next-integration.yml` (in
`integration-tests.yml`), Rust leg + `pytest -m requires_fluvio` Python leg.

## Examples

- `examples/fluvio/main.rs` → `fluvio_adapter`, `required-features = ["fluvio"]`
- `examples/fluvio_admin.rs` → `fluvio_admin`,
  `required-features = ["fluvio-integration-test"]` (topic administration)

## Python

`wingfoil-python` feature `fluvio = ["wingfoil/fluvio", "_common"]`.
**In `all-adapters` and in the wheel** — pure Rust; the client pulls a large
dependency tree but nothing platform-specific.

- Entry points: `fluvio_sub(graph, …)`, `fluvio_pub(stream, …)` —
  `#[pyadapter]`, in `src/adapters/fluvio.rs`. `fluvio_source` is not bound.
- Tests: `tests/test_fluvio.py` — service-free group by default,
  `@pytest.mark.requires_fluvio` group in the workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fluvio
# with a cluster available:
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fluvio-integration-test -- --test-threads=1
```
