# PostgreSQL Adapter

Time-partitioned historical reads (`postgres_read`), real-time live tailing
(`postgres_sub`), and streaming writes (`postgres_write`) against a PostgreSQL
database, using the async `tokio-postgres` client.

## Module Structure

```
postgres/
  mod.rs               # PostgresConnection, quote_ident/quote_table, ToSql/Row/Type re-exports
  read.rs              # postgres_read() producer, PostgresDeserialize, PostgresRowExt, postgres_timestamp()
  sub.rs               # postgres_sub() real-time producer, postgres_notify_trigger_sql()
  write.rs             # postgres_write() consumer, PostgresSerialize, PostgresWriteOperators
  integration_tests.rs # Integration tests (requires Docker, gated by feature)
  CLAUDE.md            # This file
```

## Key Design Decisions

### Time is on-graph, never in the record

Time lives in the graph tuple `(NanoTime, T)`, not inside `T`. On read,
`PostgresDeserialize::from_row` extracts the timestamp column (via
`PostgresRowExt::get_nanotime`) and returns it as the tuple's first element. On write,
`postgres_write` prepends the graph timestamp as the first inserted column, so the
target table must be laid out as `(timestamp, <business columns in to_params() order>)`.
Record structs hold only business data. This mirrors the KDB+ adapter.

### Shared time-slicing logic

`postgres_read` computes its query slices with
`crate::adapters::time_slice::compute_validated_time_slices` — the exact same routine the
KDB+ adapter uses (validation of period/start/`RunFor` included). The run's `[start, end)`
window (from `RunMode::HistoricalFrom` + `RunFor::Duration`) is split into contiguous,
half-open slices of length `period`, clamped so no slice straddles a midnight boundary,
and `query_fn` is called once per slice with `((t0, t1), date, iteration)`.
Callers filter on `time >= t0 AND time < t1` and `ORDER BY time`. `date` is the KDB-style
date integer — **days since 2000-01-01**, not the Unix epoch (documented on `postgres_read`).

Unlike KDB+ (where the time column is time-of-day within a date partition, so ordering is
checked per slice), PostgreSQL rows carry full timestamps, so the monotonic-ordering check
spans the whole read.

### Out-of-window rows are dropped, not emitted

The first slice starts at the period boundary at or before `start_time`, so when
`start_time` is not period-aligned the caller's `time >= t0` filter over-reads rows before
`start_time` (and the final slice's `t1` can reach past `end_time`). `postgres_read` clamps
each slice to the effective window `[max(t0, start_time), min(t1, end_time))` and **drops**
rows outside it, logging a single per-slice `warn!` with the dropped count. The query still
uses the period-aligned `(t0, t1)` for clean boundaries. This mirrors the KDB adapter fix:
emitting a row before the graph clock aborts a historical run, and a row past `end_time`
would drive the monotonic check to reject a later slice.

### Parameterised writes, quoted identifiers

`PostgresSerialize::to_params` returns owned, boxed `ToSql` values. `postgres_write` binds
them positionally into a prepared `INSERT INTO <table> VALUES ($1, …, $N)` statement, with the
timestamp bound as `$1`. Values are never string-formatted into SQL; the table name — the one
identifier that must be spliced — is quoted per dot-separated segment via `quote_table`
(so mixed-case and reserved-word names work). The statement is prepared once (on the first
burst) and reused; within a burst all inserts are launched concurrently so tokio-postgres
pipelines them over the connection (~1 round trip per burst instead of N).

### Live tail — `postgres_sub` (LISTEN/NOTIFY as wake-up, never as data)

`postgres_sub(conn, channel, start_from, query_fn)` is the real-time counterpart to
`postgres_read`. NOTIFY payloads are lossy (8 KB cap, dropped on disconnect), so the
notification is only a **wake-up signal**; rows are always fetched by re-running
`query_fn(cursor)` with `time > cursor ORDER BY time`, decoded by the same
`PostgresDeserialize` impl. The handoff is race-free because `LISTEN` is issued
**before** the catch-up query (watch-before-get, same as the etcd adapter). Pending
notifications are coalesced after each drain — N inserts during a query trigger one
re-query, not N.

Requirements: `RunMode::RealTime` (bails otherwise); the time column must be
**strictly increasing** across inserts. The cursor query is `time > cursor`, so a row
stamped at or before the cursor — including one that ties the current max timestamp —
is silently dropped. `postgres_notify_trigger_sql(table, channel)` returns idempotent
SQL installing the per-statement `AFTER INSERT` trigger.

### No locks on the graph path

Both nodes are async (`produce_async` / `consume_async`): all `tokio-postgres` I/O runs on the
tokio runtime, off the graph thread, communicating via the framework's channel primitives. No
`Mutex`/`RwLock` is taken in `cycle`/`setup`.

### Timestamps

`timestamp` (without time zone) maps to `chrono::NaiveDateTime`, interpreted as UTC;
`timestamptz` maps to `chrono::DateTime<Utc>` (both via tokio-postgres' `with-chrono-0_4`
feature). `get_nanotime` accepts either and returns a UTC `NanoTime`. `postgres_timestamp`
formats a `NanoTime` back to a `timestamp` literal (microsecond precision) for `WHERE` bounds.

### Python bindings fail loudly

`py_postgres_read` dispatches each column on its SQL type from the row metadata; an
unsupported type (`numeric`, `uuid`, `json`, …) is an error on the first row — never a
silent `None`. `py_postgres_write` errors on a missing dict key, an unsupported declared
type name, or a wrong-typed value (via `try_map`, aborting the run); an explicit Python
`None` writes a typed SQL NULL. Declared type names select the parameter width and must
match the column: `"int"` is int4, `"long"` is int8 (mirrors the KDB binding).

## Pre-Commit Requirements

1. **Run integration tests (requires Docker):**

   ```bash
   cargo test --features postgres-integration-test -p wingfoil \
     -- --test-threads=1 postgres::integration_tests
   ```

2. **Run standard checks** (the lint aliases mirror CI, including `-D warnings`):

   ```bash
   cargo fmt --all
   cargo lint        # default features
   cargo lint-all    # all features
   cargo test -p wingfoil
   ```

## Integration Test Details

Tests use `testcontainers` (`SyncRunner`) to start a `postgres:16-alpine` container per test.
Docker must be running. Run with `--test-threads=1` to avoid port contention.

Feature flag: `postgres-integration-test` (implies `postgres`).

| Test | What it proves |
|------|----------------|
| `test_connection_refused` | Error propagates when postgres is unreachable |
| `test_read_time_sliced` | Seeded rows are read back across hourly slices, time-ordered |
| `test_read_timestamptz` | `timestamptz` columns convert via the `DateTime<Utc>` branch of `get_nanotime` |
| `test_read_drops_rows_before_start` | An unaligned start over-reads pre-start rows; they are dropped, not emitted (run succeeds) |
| `test_read_empty_table` | Empty table yields 0 rows |
| `test_write_round_trip` | `postgres_write` rows are readable via direct query and the adapter |
| `test_write_burst_multi_row` | A multi-record burst inserts all rows at the shared timestamp |
| `test_sub_catch_up_then_live_inserts` | `postgres_sub` emits seeded rows via catch-up, then live inserts via NOTIFY |

## Gotchas

- `tokio-postgres` is pinned to `"0.7"` with the `with-chrono-0_4` feature for `NaiveDateTime`.
- The write table's column order must be `(time, <to_params() order>)`; `postgres_write` uses
  positional `VALUES`, so column names are not checked — a mismatch surfaces as a type error at
  insert time.
- `postgres_read` requires `RunMode::HistoricalFrom` (non-zero start) and `RunFor::Duration`;
  `Forever`/`Cycles` are rejected because the slice set would be unbounded.
