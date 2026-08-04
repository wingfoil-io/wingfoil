# Adapters (wingfoil)

Index and shared conventions for the I/O adapters under
`crates/wingfoil/src/adapters/`. Each adapter also has its own
`CLAUDE.md` — see the table below.

> These are **wingfoil** adapters, built on the Op pattern. The legacy
> `legacy/wingfoil/src/adapters/<name>/CLAUDE.md` files describe a different
> implementation (`#[node]` / `MutableNode` / `Rc<dyn Stream<T>>`) and are the
> *parity oracle*, not a description of this code. Do not treat them as
> interchangeable.

## Where each adapter's CLAUDE.md lives

Every adapter gets `src/adapters/<name>/CLAUDE.md`, whether its code is a
single file or a directory. That keeps one path shape across the tree (and the
same shape legacy uses, so the cutover does not move doc paths); for a
single-file adapter the directory holds only the doc. `kdb.rs` + `kdb/` and
`zmq.rs` + `zmq/` already coexist that way.

| Adapter | Code | Feature | Legacy twin |
|---|---|---|---|
| [aeron](aeron/CLAUDE.md) | `aeron/` | `aeron` or `aeron-rs` | yes |
| [augurs](augurs/CLAUDE.md) | `augurs.rs` | `augurs` | yes |
| [cache](cache/CLAUDE.md) | `cache.rs` | `cache` | module yes, CLAUDE.md no |
| [csv](csv/CLAUDE.md) | `csv.rs` | `csv` | yes |
| [etcd](etcd/CLAUDE.md) | `etcd.rs` | `etcd` | yes |
| [fix](fix/CLAUDE.md) | `fix.rs` | `fix` | yes |
| [fluvio](fluvio/CLAUDE.md) | `fluvio.rs` | `fluvio` | yes |
| [iceoryx2](iceoryx2/CLAUDE.md) | `iceoryx2/` | `iceoryx2` | yes |
| [kafka](kafka/CLAUDE.md) | `kafka.rs` | `kafka` | yes |
| [kdb](kdb/CLAUDE.md) | `kdb.rs` + `kdb/` | `kdb` | yes |
| [lines](lines/CLAUDE.md) | `lines.rs` | none (`async` for replay) | **wingfoil-only** |
| [market](market/CLAUDE.md) | `market.rs` | `market` | **wingfoil-only** |
| [otlp](otlp/CLAUDE.md) | `otlp.rs` | `otlp` | yes |
| [postgres](postgres/CLAUDE.md) | `postgres.rs` | `postgres` | yes |
| [prometheus](prometheus/CLAUDE.md) | `prometheus.rs` | `prometheus` | yes |
| [redis](redis/CLAUDE.md) | `redis.rs` | `redis` | yes |
| [web](web/CLAUDE.md) | `web/` | `web` (+ `web-tls`) | yes |
| [zmq](zmq/CLAUDE.md) | `zmq.rs` + `zmq/` | `zmq` | yes |

`common.rs` is not an adapter: it holds the shared `Sym`/`SymbolInterner` and
`TimeWindow`/`WindowFilter` (always compiled) and the `compute_time_slices` /
`compute_validated_time_slices` slicer (gated
`#[cfg(any(feature = "postgres", feature = "kdb"))]`) used by the
time-partitioned readers. A third time-sliced reader **widens that gate**, it
does not copy the helpers.

`Sym` is the tree's one interned-symbol type. It began in `kdb.rs` and moved
here when `market` needed the same thing; `kdb` re-exports it so `kdb::Sym`
still resolves. Equality is by **content**, not pointer — interners are `&mut`
and short-lived, so `Arc::ptr_eq` alone would give false negatives. A third
adapter needing interned symbols **uses this one**; it does not add a second.

## Conventions that hold for all of them

- **Out of the prelude.** Users opt in per adapter with
  `use wingfoil::adapters::<name>::…;`, mirroring `stats`.
- **Sources are free functions** taking `&GraphBuilder` first; **sinks are
  extension traits** on `Stream<Burst<T>>` (often with a `Stream<T>`
  convenience impl) returning `Stream<()>`. Legacy's free-fn-*and*-operator-
  trait pairs collapse into the trait (deviation register D1).
- **Live, never-closing sources reject `RunMode::HistoricalFrom` at wiring**
  and return `Result` — the historical channel receiver block-collects the
  whole stream up front, so an unbounded producer would deadlock at `start`
  (register B2). Only finite, timestamped sources replay historically.
- **I/O is established at `start()`, not at wiring** (register A1/A4):
  `source_at_start` for sync-thread sources, `produce_async`'s deferred spawn
  for async ones, lazy connect inside the `consume_async` consumer for sinks.
  Wiring stays pure — parse, validate, reject the wrong run mode.
- **The graph owns the tokio runtime** (register A5,
  `docs/runtime-ownership.md`). No factory takes a `&tokio::runtime::Handle`.
  Any adapter using `consume_async` inherits the `block_on` footgun (A5a): the
  graph must be built, run and dropped from a **non-async** thread.
- **No locks on the graph execution path.** `RefCell` for graph-thread-local
  state, the channel layer or `produce_async`/`consume_async` to talk to
  background threads, `ArcSwap` for a value a background thread reads ad hoc.
- **Credentials never reach error context.** Connection configs carrying a
  secret expose `redacted()` and use it at every error site
  (`PostgresConnection`, `RedisConnection`, `KdbConnection`), pinned by a unit
  test.
- **The `# Deviations from legacy` block in each module's `//!` header is the
  canonical deviation list**, with `docs/deviation-register.md` for the
  cross-cutting rows. These `CLAUDE.md` files summarise; they do not replace.

## Tests, by tier

1. `tests/<name>_adapter.rs`, `#![cfg(feature = "<name>")]` — no service
   required. Runs in `rust-test.yml`'s `test` job
   (`cargo nextest run --manifest-path crates/wingfoil/Cargo.toml --all-features --lib --tests
   -E 'not binary(/_integration$/)'`).
2. `tests/<name>_integration.rs`, `#![cfg(feature = "<name>-integration-test")]`
   — needs a service (testcontainers, an external instance, or real sockets).
   Compiled but **not run** by `test`; each has its own
   `.github/workflows/<name>-integration.yml`, registered in
   `integration-tests.yml`.
3. Python: `crates/wingfoil-python/tests/test_<name>.py`. The
   service-free group runs by default in `python-test.yml`; a
   `@pytest.mark.requires_<name>` group is deselected by `addopts` and runs in
   the adapter's own workflow.

`augurs`, `csv`, `lines`, `market` and `cache` have no tier 2 — no service to
stand up.

`market` is also the one adapter with **no venue code of its own**: it is the
shared vocabulary that out-of-tree venue adapter crates normalise into. See
[market/CLAUDE.md](market/CLAUDE.md) for what such a crate owes the contract.

## Skills

`/new-adapter` and `/bind-adapter` (`.claude/commands/`) carry the
step-by-step recipes and are **living documents**: if changing an adapter
surfaces a rule they don't capture, fold it back in the same PR.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all                                    # or, if aeron's C deps block it:
cargo clippy --manifest-path crates/wingfoil/Cargo.toml --all-features --all-targets -- -D warnings
cargo test --manifest-path crates/wingfoil/Cargo.toml --features <name>
```
