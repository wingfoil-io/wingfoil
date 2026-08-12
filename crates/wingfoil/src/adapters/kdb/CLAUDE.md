# KDB+ Adapter (wingfoil)

KDB+/q connectivity over the async `kdbplus` IPC client (`QStream`):
time-partitioned historical **reads** and their file-cached twin, a real-time
tickerplant **subscription**, and a streaming insert **sink**. Ports legacy
`wingfoil::adapters::kdb` onto the Op model.

## Layout

```
adapters/
  kdb.rs                 # module root: Sym, SymbolInterner, KdbConnection/Credentials,
                         #   re-exports (K, KdbError, qtype, CacheConfig), the module docs
  kdb/
    read.rs              # KdbExt / Rows / Row / RowIter, KdbDeserialize, kdb_read
    read_cached.rs       # kdb_read_cached
    sub.rs               # kdb_sub (tickerplant tail)
    write.rs             # KdbSerialize, KdbSinkOps
    CLAUDE.md            # this file
```

`kdb.rs` re-exports the public surface, so callers write
`use wingfoil::adapters::kdb::*;` and never name the submodules.

## Feature gating

```toml
kdb = ["dep:kdb-plus-fixed", "dep:chrono", "dep:async-stream", "cache"]
kdb-integration-test = ["kdb"]
```

Note `kdb` pulls in [`cache`](../cache/CLAUDE.md) (and with it
`sha2`/`bincode`/`serde`/`tokio/fs`/`async`) for `kdb_read_cached`.
`kdb-integration-test` adds **no** container dependency — see Tests.

The shared slicer in `adapters/common.rs` is gated
`#[cfg(any(feature = "postgres", feature = "kdb"))]`; kdb *widened* postgres's
gate rather than duplicating the helpers.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `kdb_read(g, params, conn, period, query_fn, buffer_size)` | source | bounded historical replay, one query per slice |
| `kdb_read_cached(g, params, conn, period, cache_config, query_fn)` | source | same, file-cached; `CacheConfig` **replaces** `buffer_size` (legacy signature) |
| `kdb_sub(g, run_mode, conn, table, symbols)` | source | realtime tickerplant tail |
| `KdbSinkOps::kdb_write(conn, table, buffer_size)` | sink trait | on `Stream<Burst<T>>` |

Serde traits: `KdbDeserialize` (row → `(NanoTime, T)`) and `KdbSerialize`
(`T` → row `K`). Row access: `KdbExt` / `Rows` / `Row` (`get`,
`get_timestamp`, `get_sym`). Interning: `Sym` / `SymbolInterner`.

## What to know before changing it

- **Time lives on-graph, in `(NanoTime, T)` tuples — never in the record
  struct.** `KdbDeserialize::from_kdb_row` extracts it into the tuple;
  `kdb_write` prepends it as the first inserted column. **`to_kdb_row()` must
  NOT include a time field** — it is added automatically.
- **The caller builds the whole query.** `query_fn((t0, t1), date, iteration)`
  gets a half-open `[t0, t1)` window; use `time >= t0j, time < t1j` for clean
  round-number boundaries, and add `xasc` — a non-monotonic timestamp aborts
  the run. `date` is the **KDB date integer, days since 2000-01-01**;
  `iteration` is the slice index within that day.
- **Rows outside the run's `[start_time, end_time)` are dropped** with a
  per-slice warning, via `WindowFilter` from `adapters::common`. The first
  slice starts at the period boundary at or before `start_time`, so a
  `time >= t0j` filter legitimately returns earlier rows; emitting them would
  drive the monotonic clock backwards.
- **`prev_time` is reset each slice** so time-of-day columns work across date
  partitions (timestamps restart at midnight on each new date).
- **Rows sharing a timestamp ride one `Burst<T>`.** Iterate the burst;
  `.collapse()` keeps only the last row per tick and silently drops the rest.
- **Slices are queried lazily, one at a time** (an `async_stream` generator —
  legacy's `chunk_stream` shape), so `buffer_size` is *not* inert: `Some(n)`
  paces slice fetches against the graph's drain, keeping memory bounded and
  pipelining KDB I/O with compute (registers **B5**, **D11**).
  `kdb_read_cached` stays unbounded like legacy but still streams lazily.
- **`kdb_read_cached` clamps on emit for hits *and* misses.** The cache key is
  the query string, which does not encode `start_time`/`end_time`, so the cache
  stores the **full** `[t0, t1)` slice. A full-hit replay opens no TCP
  connection at all. `T` must additionally be
  `serde::Serialize + Deserialize + Sync`. `bincode` is not self-describing —
  `CacheConfig::clear()` on a schema change.
- **`kdb_sub` is genuinely push-based**: `.u.sub[`table;syms]` then decode each
  pushed `` (`upd; table; data) `` message with the *same* `KdbDeserialize`
  impl — no re-query, no cursor (unlike postgres's `LISTEN`/`NOTIFY`). It tails
  from the moment of subscription and does **not** replay the tickerplant log /
  RDB buffer. Non-`upd` control messages (heartbeats, `.u.end`) are ignored.
  Realtime-only, rejected at wiring — **legacy parity** (legacy's `kdb_sub`
  also bailed unless `RunMode::RealTime`; register B2).
- **Write serialization details worth preserving** (each has a unit test in
  `write.rs`): non-finite floats map to q's native null/infinity literals
  (`0n`/`0w`/`0Ne`/…); symbols go through the `` `$"…" `` string cast so
  special characters survive; serialized columns must be **scalar atoms**
  (vector/nested columns are read-only) and an unsupported type names that
  limitation; a ragged burst is an error, not a panic.
- **`KdbConnection::redacted()` returns `host:port`** — credentials are used
  only at the `QStream::connect` call site and never reach an error message
  (register **D10**), pinned by `test_redacted_never_leaks_password`.
- The sink connects lazily inside the `consume_async` consumer (A1/A4) and
  `block_on`s at teardown ⇒ build, run and drop the graph from a **non-async**
  thread (A5a).

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `kdb.rs` — five items:
graph-owned runtime with `RunParams`/`RunMode` params (A5); reader defers
connect + queries to the run and streams lazily (A1/B5); sink-as-trait fold
with lazy connect (D1/A1); the live subscription's historical rejection moved
from run-start to wiring (B2, ratified); and `buffer_size` on `kdb_read` now
being real back-pressure (D11). Every legacy capability is preserved,
including `KdbExt`, `Sym`/`SymbolInterner`, and `Row`/`Rows`.

**kdb deliberately keeps legacy's separate `kdb_read`/`kdb_sub` shape** — a
unified `kdb_source` is a possible follow-up, *not* a parity gap: the two are
genuinely different mechanisms (a time-sliced historical query vs a
tickerplant push tail). See register B2.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/kdb_adapter.rs` | `#![cfg(feature = "kdb")]` | nothing |
| `tests/kdb_integration.rs` | `#![cfg(feature = "kdb-integration-test")]` | a live q instance |

KDB+ has **no public, freely-licensed container image**, so the integration
tier is skill step 10's *Option B*: it probes an externally-provided instance
(`KDB_TEST_HOST` / `KDB_TEST_PORT`, default `localhost:5000`) and skips when
unreachable. That is why `kdb-integration-test` has no `testcontainers`.

```sh
q -p 5000
```

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features kdb --test kdb_adapter
cargo test --manifest-path crates/wingfoil/Cargo.toml --features kdb-integration-test -- --test-threads=1
```

`tests/kdb_integration.rs` ports **both** legacy files —
`integration_tests.rs` and `cache_integration_tests.rs`.

> **Test-window gotcha, learned here.** `NanoTime::from_kdb_timestamp(i * 1e9)`
> lands in the **year 2000**, so a fixture stamped that way falls entirely
> outside a `HistoricalFrom(NanoTime::ZERO)` + `RunFor::Duration(short)` window
> and the round-trip count silently comes up short (the write test once
> delivered 2 of 5 rows). Either start the window at the data, or — for a
> finite self-closing feed — use `RunFor::Forever` so `[0, MAX]` covers any
> epoch. Match the legacy test's window when porting.

**Workflow:** `.github/workflows/kdb-integration.yml` (in
`integration-tests.yml`). It builds the image in `docker/` beside this file —
one container serves both the Rust leg and the `pytest -m requires_kdb` Python
leg. KDB+ publishes no freely-licensed image, so the context carries the `q`
binary and `q.k`, and CI supplies the licence from the `KDB_LICENSE_B64`
secret. The legacy tree has a byte-identical copy for its own workflow; that
copy dies with `legacy/`, this one is the survivor.

## Examples

`required-features = ["kdb"]`, each a directory with a README:

- `examples/kdb/read/main.rs` → `kdb_read`
- `examples/kdb/read_cached/main.rs` → `kdb_read_cached`
- `examples/kdb/round_trip/main.rs` → `kdb_round_trip`

## Python

`wingfoil-python` feature `kdb = ["wingfoil/kdb", "_common"]`.
**In `all-adapters` and in the wheel** — pure Rust (`kdb-plus-fixed` over TCP).

- Entry points, `#[pyadapter]` in `src/adapters/kdb.rs`: `kdb_read`, `kdb_sub`,
  `kdb_write`. `kdb_read_cached` is **not** bound.
- Dynamic payloads: reads decode into `PyKdbRow` (dispatching on each value's
  *actual* KDB type via `k.get_type()`, since kdb tags every value) → a Python
  `dict`; writes take a declared `columns` list and marshal a `dict` into a
  `PyKdbWriteRow`. Both are plain Rust data — no `Py<PyAny>` crosses to the
  worker thread. An unsupported column type is a **loud error**, never a
  `format!("{v:?}")` fallback.
- The binding names `K` / `qtype` through **`adapters::kdb`'s re-exports**, not
  a direct dependency, so it is pinned to whatever version the engine builds
  against. If a decoder needs something new, add the `pub use` in `kdb.rs`.
- Tests: `tests/test_kdb.py` — service-free group by default,
  `@pytest.mark.requires_kdb` group in the workflow above. Its `_q` helper
  speaks ~30 lines of the q wire protocol for **setup only**; every value
  assertion goes back through the adapter under test.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil/Cargo.toml --features kdb
# with `q -p 5000` running:
cargo test --manifest-path crates/wingfoil/Cargo.toml --features kdb-integration-test -- --test-threads=1
```
