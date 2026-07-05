# PostgreSQL Adapter

Time-partitioned reads (`postgres_read`) and streaming writes (`postgres_write`)
against a PostgreSQL database, using the async `tokio-postgres` client.

## Module Structure

```
postgres/
  mod.rs               # PostgresConnection, ToSql/Row re-exports, module doc
  read.rs              # postgres_read() producer, PostgresDeserialize, PostgresRowExt, postgres_timestamp()
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

`postgres_read` computes its query slices with `crate::adapters::time_slice::compute_time_slices`
— the exact same routine the KDB+ adapter uses. The run's `[start, end)` window (from
`RunMode::HistoricalFrom` + `RunFor::Duration`) is split into contiguous, half-open,
per-day slices, and `query_fn` is called once per slice with `((t0, t1), date, iteration)`.
Callers filter on `time >= t0 AND time < t1` and `ORDER BY time`.

Unlike KDB+ (where the time column is time-of-day within a date partition, so ordering is
checked per slice), PostgreSQL rows carry full timestamps, so the monotonic-ordering check
spans the whole read.

### Parameterised writes (no SQL injection)

`PostgresSerialize::to_params` returns owned, boxed `ToSql` values. `postgres_write` binds
them positionally into a prepared `INSERT INTO <table> VALUES ($1, …, $N)` statement, with the
timestamp bound as `$1`. Values are never string-formatted into SQL. The statement is prepared
once (on the first record) and reused.

### No locks on the graph path

Both nodes are async (`produce_async` / `consume_async`): all `tokio-postgres` I/O runs on the
tokio runtime, off the graph thread, communicating via the framework's channel primitives. No
`Mutex`/`RwLock` is taken in `cycle`/`setup`.

### Timestamps

`timestamp` (without time zone) maps to `chrono::NaiveDateTime` (enabled via tokio-postgres'
`with-chrono-0_4` feature). `get_nanotime` reads that as UTC `NanoTime`. `postgres_timestamp`
formats a `NanoTime` back to a `timestamp` literal (microsecond precision) for `WHERE` bounds.
Use `timestamp` columns, not `timestamptz`, to match this mapping.

## Pre-Commit Requirements

1. **Run integration tests (requires Docker):**

   ```bash
   cargo test --features postgres-integration-test -p wingfoil \
     -- --test-threads=1 postgres::integration_tests
   ```

2. **Run standard checks:**

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features
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
| `test_read_empty_table` | Empty table yields 0 rows |
| `test_write_round_trip` | `postgres_write` rows are readable via direct query and the adapter |
| `test_write_burst_multi_row` | A multi-record burst inserts all rows at the shared timestamp |

## Gotchas

- `tokio-postgres` is pinned to `"0.7"` with the `with-chrono-0_4` feature for `NaiveDateTime`.
- The write table's column order must be `(time, <to_params() order>)`; `postgres_write` uses
  positional `VALUES`, so column names are not checked — a mismatch surfaces as a type error at
  insert time.
- `postgres_read` requires `RunMode::HistoricalFrom` (non-zero start) and `RunFor::Duration`;
  `Forever`/`Cycles` are rejected because the slice set would be unbounded.
