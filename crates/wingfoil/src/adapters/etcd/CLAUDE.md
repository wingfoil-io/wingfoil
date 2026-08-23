# etcd Adapter (wingfoil)

A streaming key-prefix snapshot + live watch **source** and a key-value PUT
**sink** for etcd. Ports legacy `wingfoil::adapters::etcd` onto the Op model.

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
| `EtcdSinkOps::etcd_pub(conn)` | sink trait | the defaults — unleased, overwriting; on `Stream<Burst<EtcdEntry>>` **and** `Stream<EtcdEntry>` |
| `EtcdSinkOps::etcd_pub_with_options(conn, EtcdPubOptions)` | sink trait | the required method; `etcd_pub` is a provided method forwarding `EtcdPubOptions::default()` |

Config types: `EtcdConnection` (`new` / `with_endpoints`, plus `From<&str>` /
`From<String>` / `From<&String>`), `EtcdPubOptions`, `EtcdEntry`, `EtcdEvent`,
`EtcdEventKind`.

`EtcdPubOptions` is plain public fields + a hand-written `Default`
(`lease_ttl: None`, `force: true`), the `FixOptions` shape — callers write
`EtcdPubOptions { force: false, ..EtcdPubOptions::default() }`. **`Default` is
legacy's `(None, true)` and the Python binding's `lease_ttl_secs=None,
force=True`; a unit test in `etcd.rs` pins it.** Flipping either default would
silently change every call site that names neither.

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
- **The snapshot shares one timestamp, but that does not make it one burst.**
  The stamp is right (one consistent GET at `snapshot_rev`), yet this source is
  realtime-only and the *realtime* channel receiver groups by **arrival** — a
  cycle emits whatever `try_recv` drains right then — while only the historical
  receiver groups by timestamp. The producer sends one value per `send_at` and
  the first send already wakes the kernel, so a multi-key snapshot can be split
  across cycles. Nothing is lost, but **never bound an `etcd_sub` test or
  consumer with `RunFor::Cycles(n)`** when it must see every event: use
  `RunFor::Duration` and `collapse_accumulate`. This flaked the integration
  suite twice — once "fixed" by giving the snapshot a single timestamp, which
  cannot help in the only run mode this adapter supports. `Cycles(n)` is also a
  **hang** risk here: if events coalesce into fewer bursts than `n`, a realtime
  run with nothing scheduled parks on the ready channel indefinitely.
- **The producer task spawns in `start()`**, deferred via `source_at_start`, so
  the connect + watch happen at run start with node context, not at wiring
  (register A1/A4).
- **The sink connects lazily on the first write**, inside the `consume_async`
  consumer — so wiring opens no socket, and a connect or `lease_grant` failure
  surfaces *during* the run. An empty stream connects and leases nothing.
- **`force: false` must still abort a single-cycle run.** The conditional write
  (`create_revision == 0`) returns an error which `consume_async` surfaces on a
  later cycle — or, for the **final** write, via the `flush` teardown wired
  here as `finally`. That teardown path is exactly how legacy's
  `AsyncConsumerNode::teardown` aborts a `RunFor::Cycles(1)` run, and it is
  what made the per-write `block_on` removable (register B1). If you touch the
  sink's teardown, re-check `RunFor::Cycles(1)` + `force: false`.
- **Leases are revoked at teardown**, not left to expire — that graph-thread
  `block_on` is the one remaining `block_on` here (register A5a): **build, run
  and drop the graph from a non-async thread**.
- The keepalive task renews every `ttl/3` via `tokio::spawn`.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `etcd.rs` —
(1) the graph owns the tokio runtime and `etcd_sub` takes a `RunMode`
(register A5); (2) the sink connects lazily on the first write (A1/A4);
(3) the sink is a **trait only** — legacy had both a free `etcd_pub` and an
`EtcdPubOperators` trait (register D1); (4) the sink's options are an
`EtcdPubOptions` struct rather than legacy's positional
`(lease_ttl: Option<Duration>, force: bool)` — a readability change only, the
defaults are legacy's (issue #459). Every legacy capability
(snapshot→watch, deletes, leases with keepalive and revoke-on-shutdown, the
`force` conditional write) is preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/etcd_adapter.rs` | `#![cfg(feature = "etcd")]` | nothing |
| `tests/etcd_integration.rs` | `#![cfg(feature = "etcd-integration-test")]` | an etcd container (testcontainers) |

```bash
cargo test -p wingfoil --features etcd --test etcd_adapter
cargo test -p wingfoil --features etcd-integration-test -- --test-threads=1
```

Local service:

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

**Workflow:** `.github/workflows/etcd-integration.yml` (registered in
`integration-tests.yml`), which runs the Rust leg **and** a Python leg
(`pytest -m requires_etcd`).

## Example

`examples/etcd_adapter.rs`, `required-features = ["etcd"]`.

## Python

`wingfoil-python` feature `etcd = ["wingfoil/etcd", "_common"]`.
**In `all-adapters` and in the wheel** — pure Rust (etcd-client over tonic),
but it needs `protoc` at build time to compile the etcd protos, which
`pypi-publish.yml` installs.

- Entry points: `etcd_sub(graph, …)`, `etcd_pub(stream, …)` — `#[pyadapter]`,
  in `src/adapters/etcd.rs`. `EtcdPubOptions` stays **flat keyword arguments**
  on the Python side (`lease_ttl_secs=None, force=True`), assembled into the
  struct in the binding — the `ws`/`WsConfig` shape.
- Tests: `tests/test_etcd.py` — service-free group by default,
  `@pytest.mark.requires_etcd` group in the workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil --features etcd
# with a container available:
cargo test -p wingfoil --features etcd-integration-test -- --test-threads=1
```
