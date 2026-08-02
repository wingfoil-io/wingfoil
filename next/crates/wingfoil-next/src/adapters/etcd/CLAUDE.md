# etcd Adapter (wingfoil-next)

A streaming key-prefix snapshot + live watch **source** and a key-value PUT
**sink** for etcd. Ports classic `wingfoil::adapters::etcd` onto the Op model.

This was the **first async adapter ported**, so several of the cross-cutting
async rules (live-source rejection, graph-owned runtime, lazy sink connect)
were written here first and every later adapter's docs point back at it.

## Layout

```
adapters/
  etcd.rs          # connection/entry/event types, etcd_sub, EtcdSinkOps
  etcd/CLAUDE.md   # this file
```

## Feature gating

```toml
etcd = ["dep:etcd-client", "dep:async-stream", "async"]
etcd-integration-test = ["etcd", "dep:testcontainers"]
```

`etcd` is also a *pluggable backend* for [`zmq`](../zmq/CLAUDE.md): with both
features on, `EtcdRegistry` becomes a `ZmqRegistry` discovery backend.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `etcd_sub(g, run_mode, conn, prefix)` | source | `Result<Stream<Burst<EtcdEvent>>>` — snapshot then watch |
| `EtcdSinkOps::etcd_pub(conn, lease_ttl, force)` | sink trait | on `Stream<Burst<EtcdEntry>>` **and** `Stream<EtcdEntry>` |

Config types: `EtcdConnection` (`new` / `with_endpoints`, plus `From<&str>` /
`From<String>` / `From<&String>`), `EtcdEntry`, `EtcdEvent`, `EtcdEventKind`.

## What to know before changing it

- **`etcd_sub` is realtime-only and rejects `RunMode::HistoricalFrom` at
  wiring** (register B2, ratified). The watch never closes and the historical
  channel receiver block-collects the whole stream up front, so a historical
  run would deadlock at `start` — this is the etcd bug the port fixed. The
  `run_mode` parameter exists *only* for that check.
- **Watch-before-GET.** The watch is opened first, then the snapshot is read
  with its revision; any watch event with `mod_revision <= snapshot_rev` is
  filtered as a duplicate. That is what makes the handoff race-free — the
  redis Streams reader mirrors it with stream IDs. Do not reorder.
- **Everything is stamped `NanoTime::now()`.** It is a live wall-clock stream;
  there is no historical timeline.
- **The producer task spawns in `start()`**, deferred via `source_at_start`, so
  the connect + watch happen at run start with node context, not at wiring
  (register A1/A4).
- **The sink connects lazily on the first write**, inside the `consume_async`
  consumer — so wiring opens no socket, and a connect or `lease_grant` failure
  surfaces *during* the run. An empty stream connects and leases nothing.
- **`force: false` must still abort a single-cycle run.** The conditional write
  (`create_revision == 0`) returns an error which `consume_async` surfaces on a
  later cycle — or, for the **final** write, via the `flush` teardown wired
  here as `finally`. That teardown path is exactly how classic's
  `AsyncConsumerNode::teardown` aborts a `RunFor::Cycles(1)` run, and it is
  what made the per-write `block_on` removable (register B1). If you touch the
  sink's teardown, re-check `RunFor::Cycles(1)` + `force: false`.
- **Leases are revoked at teardown**, not left to expire — that graph-thread
  `block_on` is the one remaining `block_on` here (register A5a): **build, run
  and drop the graph from a non-async thread**.
- The keepalive task renews every `ttl/3` via `tokio::spawn`.

## Deviations from classic

Canonical list: the `# Deviations from classic` block in `etcd.rs` —
(1) the graph owns the tokio runtime and `etcd_sub` takes a `RunMode`
(register A5); (2) the sink connects lazily on the first write (A1/A4);
(3) the sink is a **trait only** — classic had both a free `etcd_pub` and an
`EtcdPubOperators` trait (register D1). Every classic capability
(snapshot→watch, deletes, leases with keepalive and revoke-on-shutdown, the
`force` conditional write) is preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/etcd_adapter.rs` | `#![cfg(feature = "etcd")]` | nothing |
| `tests/etcd_integration.rs` | `#![cfg(feature = "etcd-integration-test")]` | an etcd container (testcontainers) |

```bash
cargo test -p wingfoil-next --features etcd --test etcd_adapter
cargo test -p wingfoil-next --features etcd-integration-test -- --test-threads=1
```

Local service:

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

**Workflow:** `.github/workflows/etcd-next-integration.yml` (registered in
`integration-tests.yml`), which runs the Rust leg **and** a Python leg
(`pytest -m requires_etcd`).

## Example

`examples/etcd_adapter.rs`, `required-features = ["etcd"]`.

## Python

`wingfoil-next-python` feature `etcd = ["wingfoil-next/etcd", "_common"]`.
**In `all-adapters` and in the wheel** — pure Rust (etcd-client over tonic),
but it needs `protoc` at build time to compile the etcd protos, which
`pypi-publish.yml` installs.

- Entry points: `etcd_sub(graph, …)`, `etcd_pub(stream, …)` — `#[pyadapter]`,
  in `src/adapters/etcd.rs`.
- Tests: `tests/test_etcd.py` — service-free group by default,
  `@pytest.mark.requires_etcd` group in the workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features etcd
# with a container available:
cargo test -p wingfoil-next --features etcd-integration-test -- --test-threads=1
```
