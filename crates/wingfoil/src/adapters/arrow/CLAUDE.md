# Arrow Adapter (wingfoil)

A serde-typed Apache Arrow IPC **streaming-format** (`.arrows`) file adapter —
a lazy, bounded historical replay **source** over a file or a directory tree,
and a flush-per-tick, crash-tolerant file **sink** with optional Hive-style
time partitioning. **Wingfoil-only**: there is no legacy twin, so no port-plan
entry and no parity tests. The columnar cousin of [`csv`](../csv/CLAUDE.md).

It is the durable live-capture format. [`parquet`](../parquet/CLAUDE.md) (the
compact archive / backtest format) is the second user of the columnar core;
compaction is just a graph, `arrow_read(dir)` into
`parquet_write_partitioned(...)`. **A change to `columnar.rs` must keep both
adapters' suites passing** (`--features arrow` and `--features parquet`).

## Layout

```
adapters/
  arrow.rs              # public surface: options, arrow_read*, ArrowSinkOps,
                        #   the IPC BatchFileWriter (IpcFile)
  arrow/columnar.rs     # pub(crate) format-agnostic core (parquet reuses it):
                        #   trace_fields, BatchBuilder, TimePartition +
                        #   PartitionKey, BatchFileWriter, wire_batch_sink,
                        #   input_files, replay_batch_files
  arrow/CLAUDE.md       # this file
```

## Feature gating

```toml
arrow = ["dep:arrow-array", "dep:arrow-schema", "dep:arrow-ipc", "dep:serde_arrow", "async", "dep:async-stream"]
```

`async` buys the lazy, bounded replay (as for `csv`); the sink is synchronous.
`arrow-ipc` is built without its lz4/zstd codecs, so compressed streams written
elsewhere fail to decode.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `arrow_read(g, path, get_time)` | source | `Result<Stream<Burst<T>>>`; `path` is a file or a directory |
| `arrow_read_with_options(g, path, get_time, ArrowReadOptions)` | source | `buffer_size`, default `Some(1024)` |
| `ArrowSinkOps::arrow_write(path)` | sink trait (provided) | on `Stream<Burst<T>>` **and** `Stream<T>` |
| `ArrowSinkOps::arrow_write_with_options(path, ArrowWriteOptions)` | sink trait (required) | |
| `ArrowSinkOps::arrow_write_partitioned(root, TimePartition)` | sink trait (provided) | |
| `ArrowSinkOps::arrow_write_partitioned_with_options(root, TimePartition, ArrowWriteOptions)` | sink trait (required) | |
| `ArrowWriteOptions` | options | `batch_size` 1024, `flush_every_tick` true, `time_column` `Some("time")`, `file_name` `data.arrows` |
| `TimePartition` | enum | `Year` / `Month` / `Day` / `Hour`, UTC, defined in `columnar.rs` and re-exported |

Both `Default`s are pinned by unit tests in `arrow.rs`.

## What to know before changing it

- **The sink is `for_each` + `finally`, not `for_each_mut`.** `finally` is the
  only hook that runs at teardown — including after an aborted run — and it is
  where pending rows are written and the end-of-stream marker appended.
  `aborted_run_leaves_a_readable_file` and
  `sink_without_per_tick_flush_writes_at_teardown` fail if it is removed.
- **Every batch is flushed to the OS on write** (`IpcFile::write`), not
  `fsync`ed. With `flush_every_tick` (default) that is once per tick; without
  it, once per full `batch_size`, per partition switch, and at teardown.
- **A file without the end-of-stream marker still reads** (arrow-ipc treats
  EOF at a message boundary as end of stream), which is what makes a crash
  lose only the pending rows and a mid-run read possible.
- **The `time` column is `Timestamp(Nanosecond, None)`** and deserializes into
  `i64`, not `u64`/`NanoTime` (serde_arrow rejects those). A read type that
  wants the capture times names `time: i64`.
- **Extra file columns are ignored** by `serde_arrow::from_record_batch`, so no
  projection is needed; pinned by `columns_the_record_does_not_name_are_ignored`.
- **Records must be structs with named fields.** Tracing a primitive or tuple
  fails at wiring with `arrow_write: cannot trace an Arrow schema for ...`.
- **A record field named like the time column is a wiring error**
  (`... has a field named `time`, which collides with the time column`), in
  `BatchBuilder::new` — so it holds for both `arrow` and `parquet`. Before it,
  the batch silently carried two `time` columns.
- **A directory read merges the files sharing a directory by time** (one
  partition written by several runs under different `file_name`s) and replays
  directories in path order. Pinned by `files_sharing_a_partition_merge_by_time`.
- **A record that fails to serialize leaves the pending rows writable.**
  `serde_arrow` has no rollback, so `BatchBuilder` serializes each record into
  a probe builder first; a failure never touches the real one. Pinned by
  `unserializable_record_keeps_earlier_rows`.
- **One open file in a partitioned sink**, switched when the UTC partition key
  changes (graph time is monotonic). Partition files are truncated if they exist.
- **A decode error can pre-empt the last good group.** The historical receiver
  reads one group ahead to close a same-time group, so an error arriving as
  that look-ahead aborts before the group before it is delivered. That is the
  receiver's behaviour, not the adapter's.
- Error context always names `arrow_read` / `arrow_write` and the path.

## Deviations

Canonical list: the `# Deviations` block in `arrow.rs` (departures from the
`/new-adapter` conventions, since there is no legacy oracle): `for_each` +
`finally` instead of `for_each_mut`, and per-tick flush as the default.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/arrow_adapter.rs` | `#![cfg(feature = "arrow")]` | nothing |
| inline `mod tests` in `arrow.rs` / `arrow/columnar.rs` | feature | nothing |

```bash
cargo test -p wingfoil --features arrow --test arrow_adapter
cargo test -p wingfoil --features arrow --lib adapters::arrow
```

No integration tier and no dedicated workflow (fixture files are the
integration test). Runs in `rust-test.yml`'s `test` job.

## Example

`examples/adapters/arrow/` — target `arrow_adapter`,
`required-features = ["arrow"]`.

## Python

Not bound yet — follow-up (`/bind-adapter arrow`).

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil --features arrow
```
