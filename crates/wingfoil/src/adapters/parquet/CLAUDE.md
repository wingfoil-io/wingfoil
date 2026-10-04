# Parquet Adapter (wingfoil)

A serde-typed Apache Parquet (`.parquet`) file adapter — a lazy, bounded
historical replay **source** over a file or a directory tree, and a
row-grouped, compressed file **sink** with optional Hive-style time
partitioning. **Wingfoil-only**: there is no legacy twin, so no port-plan entry
and no parity tests.

It is the compact archive / backtest format, and the **second user of the
`arrow` adapter's columnar core** (`../arrow/columnar.rs`). Arrow IPC is the
live-capture format; compaction is a graph, `arrow_read(dir)` into
`parquet_write_partitioned(root, TimePartition::Day)` — pinned by
`compaction_from_arrow_capture`.

## Layout

```
adapters/
  parquet.rs            # public surface: options, ParquetCompression,
                        #   parquet_read*, ParquetSinkOps, and the
                        #   BatchFileWriter over ArrowWriter (ParquetFile)
  parquet/CLAUDE.md     # this file
  arrow/columnar.rs     # the shared core: batching, partitioning, sink
                        #   wiring, multi-file replay — change it THERE
```

## Feature gating

```toml
parquet = ["arrow", "dep:parquet"]
```

The `parquet` crate is built with default features off plus `arrow`, `snap`
and `zstd`. Files compressed with any other codec (gzip, brotli, lz4) fail to
decode, mid-stream.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `parquet_read(g, path, get_time)` | source | `Result<Stream<Burst<T>>>`; `path` is a file or a directory |
| `parquet_read_with_options(g, path, get_time, ParquetReadOptions)` | source | `batch_size` 1024, `buffer_size` `Some(1024)` |
| `ParquetSinkOps::parquet_write(path)` | sink trait (provided) | on `Stream<Burst<T>>` **and** `Stream<T>` |
| `ParquetSinkOps::parquet_write_with_options(path, ParquetWriteOptions)` | sink trait (required) | |
| `ParquetSinkOps::parquet_write_partitioned(root, TimePartition)` | sink trait (provided) | |
| `ParquetSinkOps::parquet_write_partitioned_with_options(root, TimePartition, ParquetWriteOptions)` | sink trait (required) | |
| `ParquetWriteOptions` | options | `batch_size` 1024, `row_group_size` 65 536, `compression` Snappy, `time_column` `Some("time")`, `file_name` `data.parquet` |
| `ParquetCompression` | enum, `#[non_exhaustive]` | `Uncompressed` / `Snappy` (default) / `Zstd` (default level) |
| `TimePartition` | enum | re-exported from `arrow` (defined in the core) |

Both `Default`s and the codec mapping are pinned by unit tests in `parquet.rs`.

## What to know before changing it

- **A single Parquet file is not readable mid-run.** The footer is written at
  teardown (the core's `finally`, which also runs after an aborted run). A
  crash leaves an unreadable file. In a partitioned tree each partition is
  finished when the sink moves on, so completed partitions are readable.
- **No per-tick flush, by design** (`flush_every_tick: false` in
  `sink_config`). Rows go to a batch at `batch_size`; `ArrowWriter` cuts a row
  group at `row_group_size` (`set_max_row_group_row_count`) and writes it
  as it fills. Pinned by `row_groups_prove_streaming` (10 000 rows, groups of
  1 000 → 10 groups, and the file grows mid-run).
- **The `time` column is `Timestamp(Nanosecond, None)`** and deserializes into
  `i64`, not `u64`/`NanoTime`. A read type that wants the written times names
  `time: i64`. A *written* type with a field named like the time column is a
  wiring error (core check, shared with `arrow`).
- **The reader holds a row group, not the file.** `ParquetRecordBatchReaderBuilder`
  reads the footer at open (that is the wiring-time fail-fast); row groups are
  fetched as the iterator is driven.
- **Shares everything else with `arrow`**: directory walking and per-directory
  time merge, partition keys and layout, one open file at a time, truncation,
  extra columns ignored, probe-before-push on serialize. See
  [`../arrow/CLAUDE.md`](../arrow/CLAUDE.md). A core change must keep both
  suites green.
- Error context always names `parquet_read` / `parquet_write` and the path.

## Deviations

Canonical list: the `# Deviations` block in `parquet.rs` (departures from the
`/new-adapter` conventions — there is no legacy oracle): `for_each` + `finally`
via the core, buffering between ticks, and compaction as a graph rather than an
API.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/parquet_adapter.rs` | `#![cfg(feature = "parquet")]` | nothing (`python3` + `pyarrow` optional) |
| inline `mod tests` in `parquet.rs` | feature | nothing |

```bash
cargo test -p wingfoil --features parquet --test parquet_adapter
cargo test -p wingfoil --features parquet --lib adapters::parquet
cargo test -p wingfoil --features arrow      # the core's first user
```

`partitioned_tree_is_readable_outside_the_adapter` reads the tree with the
`parquet` crate directly, and additionally with `pyarrow.dataset` (Hive
partitioning on) when `python3` has it; it skips that half with an
`eprintln!` otherwise. No integration tier and no dedicated workflow. Runs in
`rust-test.yml`'s `test` job.

## Example

`examples/adapters/parquet/` — target `parquet_adapter`,
`required-features = ["parquet"]`.

## Python

Not bound yet — follow-up (`/bind-adapter parquet`).

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil --features parquet
cargo test -p wingfoil --features arrow
```
