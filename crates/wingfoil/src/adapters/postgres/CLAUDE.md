# PostgreSQL Adapter (wingfoil)

Time-partitioned historical **reads**, a realtime `LISTEN`/`NOTIFY` live-tail
**source**, and a streaming insert **sink**, over async `tokio-postgres`. Ports
legacy `wingfoil::adapters::postgres` onto the Op model.

Two things were invented here and are now shared: the **time slicer** in
`adapters::common` and the **mode-agnostic `<adapter>_source`** shape (register
B2's agreed plan). It is also the **template for every Python binding**.

## Layout

```
adapters/
  postgres.rs          # helpers, connection, serde traits, read/sub/source, PostgresSinkOps
  postgres/CLAUDE.md   # this file
  common.rs            # TimeWindow/WindowFilter (always compiled) + the slicer (gated)
```

## Feature gating

```toml
postgres = ["dep:tokio-postgres", "dep:chrono", "dep:async-stream", "async"]
postgres-integration-test = ["postgres", "dep:testcontainers"]
```

The slicer in `common.rs` is gated
`#[cfg(any(feature = "postgres", feature = "kdb"))]`. **A third time-sliced
reader widens that gate — it does not copy the helpers.**

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `postgres_read(g, params, conn, period, query_fn, buffer_size)` | source | bounded historical replay, one query per slice |
| `postgres_sub(g, run_mode, conn, channel, start_from, query_fn)` | source | realtime live tail |
| `postgres_source(g, params, conn, cfg)` | source | **mode-agnostic** — dispatches on `RunMode` at wiring |
| `PostgresSinkOps::postgres_write(conn, table, buffer_size)` | sink trait | on `Stream<Burst<T>>` |

Supporting surface: `PostgresConnection` (+ `redacted()`),
`PostgresDeserialize` / `PostgresSerialize` / `PostgresRowExt`,
`quote_ident` / `quote_table` / `postgres_timestamp`,
`postgres_notify_trigger_sql(table, channel)`, `PostgresSourceConfig`
(`.historical(..)` / `.live(..)`).

## What to know before changing it

- **Time lives on-graph, in `(NanoTime, T)` tuples — never in the record
  struct.** Read: extracted from a timestamp column into the tuple. Write:
  prepended as the first inserted column. Record structs hold business data
  only.
- **`postgres_read` slices the run window at wiring, queries at run start.**
  `compute_validated_time_slices` splits `[start, end)` (from
  `RunMode::HistoricalFrom` + `RunFor::Duration`) into contiguous, half-open,
  **midnight-aligned** slices of length `period`; `query_fn` is called once per
  slice with `((t0, t1), date, iteration)`. The slicing is pure and fails fast;
  the connect + queries run at the start of the run via `produce_async`.
- **`date` is the KDB-style day count — days since 2000-01-01, not the Unix
  epoch** — matching the kdb adapter. `iteration` is the slice index within
  that day. This trips people up.
- **Rows outside `[start_time, end_time)` are dropped, not emitted.** The first
  slice begins at the period boundary at or *before* `start_time`, so a
  `time >= t0` filter legitimately returns earlier rows (and the last slice's
  `t1` can overshoot `end_time`); emitting them would drive the monotonic graph
  clock backwards and abort the run. `WindowFilter` from `adapters::common`
  does the clamp and logs a per-slice warning. Queries must `ORDER BY time`.
- **Slices are queried lazily, one at a time** (an `async_stream` generator),
  so with a `buffer_size` bound the replay is bounded in memory and pipelines
  query I/O with graph compute — legacy's model (register **B5**). Do not
  collect the result set up front.
- **`postgres_sub` uses `NOTIFY` as a wake-up signal only** — the payload is
  ignored and the adapter re-queries past a time cursor, so nothing is lost to
  `NOTIFY`'s payload size limit. `LISTEN` is issued **before** the catch-up
  query (watch-before-get, as `etcd_sub`), so an insert committed during
  startup is not missed. Install the trigger with
  `postgres_notify_trigger_sql`. It is realtime-only and rejects
  `HistoricalFrom` at wiring — **legacy parity**, legacy's `postgres_sub`
  already required `RunMode::RealTime` (register B2).
- **Prefer `postgres_source` at new call sites.** Supply both halves and the
  graph is fully mode-agnostic — flip real-time vs historical at `run()` with
  wiring unchanged. Supply one and the other mode errors at wiring naming the
  missing half. `postgres_read`/`postgres_sub` stay public for single-mode
  callers.
- **`PostgresConnection::redacted()` at every error site.** A DSN embeds
  `password=…`; four unit tests pin the masking (including case-insensitivity
  and the no-password no-op). This is the origin of the credential-redaction
  rule in `/new-adapter-next`.
- **The sink connects lazily inside the `consume_async` consumer** (A1/A4) and
  pipelines a whole burst's inserts over the single connection (~1 round trip
  per burst). Postgres has no per-write conditional that must abort
  synchronously, so the off-thread sink fits.
- `block_on` at teardown ⇒ build, run and drop the graph from a **non-async**
  thread (A5a).

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `postgres.rs` — four
items: graph-owned runtime and the `RunParams`/`RunMode` params (A5); the
reader defers connect + queries to the run and streams slices lazily (A1/B5);
the sink is a trait only and pipelines per burst via `consume_async` (D1, A1);
and the sink's added `buffer_size` (D3). Every legacy capability (time-sliced
read, live tail, streaming write, the three serde traits, the quoting and
timestamp helpers, password redaction) is preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/postgres_adapter.rs` | `#![cfg(feature = "postgres")]` | nothing |
| `tests/postgres_integration.rs` | `#![cfg(feature = "postgres-integration-test")]` | a Postgres container |
| `tests/common_adapter.rs` | none | nothing — covers `WindowFilter`/`TimeWindow` |

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features postgres --test postgres_adapter
cargo test --manifest-path crates/wingfoil/Cargo.toml --features postgres-integration-test -- --test-threads=1
```

```sh
docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
```

**Workflow:** `.github/workflows/postgres-next-integration.yml` (in
`integration-tests.yml`), Rust leg + `pytest -m requires_postgres` Python leg.

## Example

`examples/postgres_adapter/main.rs` → example `postgres_adapter`,
`required-features = ["postgres"]`.

## Python

`postgres` was the **first adapter bound** and is the template
`/bind-adapter-next` tells you to read first
(`crates/wingfoil-python/src/adapters/postgres.rs`). Feature:
`postgres = ["wingfoil/postgres", "dep:chrono", "_common"]` — `chrono` is
named directly because the row decoder mentions `NaiveDateTime`. **In
`all-adapters` and in the wheel.**

- Entry points, all `#[pyadapter]`: `postgres_read`, `postgres_sub`,
  **`postgres_source`**, `postgres_write`. The unified source is exposed
  because Python takes the run mode as an argument anyway.
- Dynamic payloads: `PyPgValue` / `PyPgRow` — plain Rust data with **no
  `Py<PyAny>` inside**, because rows are decoded on a worker thread and cross a
  channel.
- Reads are **lossless**: legacy `py_postgres_read` collapsed a burst to its
  last value and silently dropped rows sharing a timestamp. Next returns the
  whole burst as a Python `list`; callers write `[0]` for the single-row case.
- Tests: `tests/test_postgres.py` — service-free group by default,
  `@pytest.mark.requires_postgres` group in the workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil/Cargo.toml --features postgres
# with a container available:
cargo test --manifest-path crates/wingfoil/Cargo.toml --features postgres-integration-test -- --test-threads=1
```
