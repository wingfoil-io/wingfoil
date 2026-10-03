# polars Adapter (wingfoil)

Replay a [polars](https://pola.rs) `DataFrame` — in memory, Parquet or Arrow
IPC — as a timestamped historical source, and collect a row stream back into a
`DataFrame` or a Parquet / IPC file.

**Wingfoil-only — legacy has no polars adapter.** There is no parity oracle and
no port-plan row; the `# Deviations` block in `polars.rs` records departures
from the adapter *conventions* instead.

## Layout

```
adapters/
  polars.rs          # the whole adapter (row type, source, sink trait)
  polars/CLAUDE.md   # this file
```

## Feature gating

```toml
polars = ["dep:polars", "async", "dep:async-stream"]
```

The polars dependency is `0.54`, `default-features = false`, with `parquet`,
`ipc` and `dtype-datetime` only. `polars-arrow` (same `0.54` line,
`io_ipc` + `io_ipc_compression` — both already built by polars' `ipc`) is a
direct dependency for one thing: its IPC `FileReader`, which reads a file one
record batch at a time and which polars does not re-export. Keep the two on
the same line. **Not 0.55**: 0.55's `polars-io` needs
`sysinfo 0.39`, whose MSRV is Rust 1.95 — above the workspace's
`rust-version = "1.88"`. Raising the floor is a toolchain decision, not a
drive-by; Renovate is held `<0.55` in `.github/renovate.json` until then.
polars-io's optional `object_store` backend puts `quick-xml 0.39` in the lock
(never compiled), which `.cargo/audit.toml` ignores with the reason — drop that
ignore when polars moves past `object_store 0.13`. No `fmt` feature either, so `DataFrame`'s `Display` is a one-line
shape summary; print schemas/rows yourself (the example does).

## Entry points

| Item | Kind | Shape |
|---|---|---|
| `polars_read(g, source, time_column, buffer_size)` | source | `Result<Stream<Burst<PolarsRow>>>` |
| `PolarsSinkOps::polars_collect[_with_options]` | sink trait | `(Stream<()>, PolarsCollector)` |
| `PolarsSinkOps::polars_write[_with_options](path, …)` | sink trait | `Result<Stream<()>>` |
| `PolarsRow` | value | `Arc<Schema>` + `Vec<AnyValue<'static>>` |
| `PolarsSource` | `impl Into` param | `DataFrame`, path (by extension), `Parquet(path)`, `Ipc(path)` |
| `PolarsFormat` | enum | `Parquet` / `Ipc`, `from_path`, `read`, `write` |
| `PolarsSinkOptions` | options struct | `time_column: Option<String>` (default `"time"`), `format` |

Both sink impls exist (`Stream<Burst<PolarsRow>>` and `Stream<PolarsRow>`):
`PolarsRow` is not a `Burst`, so they cannot collide. The polars crate is
re-exported as `adapters::polars::polars` — name polars types through it.

## What to know before changing it

- **The time column becomes the tick time and is dropped from the rows.** That
  is what makes read → write reproduce the frame (the sink re-adds a
  `Datetime[ns]` time column) instead of doubling it. Accepted dtypes:
  `Datetime` (any unit, scaled to ns, tz ignored), `Int64`, `UInt64` (ns).
- **Files stream in chunks; nothing reads a whole file.** Wiring opens the
  file and reads only the footer (schema + chunk layout) — missing/unreadable
  file, missing time column and unsupported dtype are wiring `Err`s. The run
  pulls one chunk at a time over `produce_async` (`buffer_size` bounds
  look-ahead): a Parquet **row group** (`ParquetReader` + `set_metadata` with
  the wiring-time footer + `with_slice` aligned to the row group +
  `ParallelStrategy::RowGroups`, which skips every row group the slice does not
  overlap — the `Columns`/`None` strategies walk earlier row groups with an
  empty filter, so do not switch) or an IPC **record batch** (polars-arrow's
  `FileReader`). Never `finish()` a whole file on the replay path;
  `PolarsFormat::read` is a test/convenience helper, not the source.
- **Time validation is incremental for files, whole for frames.** `Clock`
  carries the absolute row index and the last time across chunks, so null,
  negative and decreasing (including across a chunk boundary) abort the run
  mid-stream naming the row and the file, like `csv_read`'s malformed row. An
  in-memory `DataFrame` is resident anyway, so its column is checked at wiring
  through the same `Clock`, then replayed in zero-copy `slice`s — one
  row-yielding path for all three sources. Do not move it to `replay_results`,
  which would copy the frame into a `Vec` of rows up front.
- **The laziness tests are proven against the old whole-file load**
  (`a_bad_time_in_a_later_chunk_does_not_stop_the_file_wiring`,
  `a_corrupt_later_row_group_is_not_read_at_wiring`,
  `a_decrease_across_a_chunk_boundary_aborts_the_run_naming_the_row` all fail
  at wiring against it). They write several small row groups / batches: a
  Parquet file needs `ParquetWriter::with_row_group_size`, and an IPC file gets
  one batch per polars chunk — build the frame with `vstack_mut`, and do not
  replace a column afterwards (that realigns the chunks into one batch).
- **The sinks write at `stop`**, via `Builder::register_op1_with_stop`: rows
  are buffered per cycle, the frame built once at the end of the run. The
  buffer is per-run `State`, so a re-run starts empty. **`stop` runs after an
  abort too** (the `Op::stop` contract), so an aborted run still collects /
  writes the rows that reached the sink before it. Skipping the write on abort
  would need an engine signal `Ctx` does not carry.
- **`polars_write` never touches `path` until the frame is fully written.** At
  wiring it only probes the directory (create + remove a scratch file), so a
  bad path fails early without truncating anything. At `stop` it encodes to a
  sibling `.<name>.<pid>.<n>.tmp` and renames over `path` on success, removing
  the temp on failure — so `path` holds either its previous contents or a
  whole, valid file (`an_aborted_write_replaces_the_file_whole` and
  `a_failed_write_leaves_no_temp_file` pin it). Do not go back to
  `File::create` at wiring: an abort then left a zero-byte Parquet file, which
  is invalid.
- **Sink schema rules**: every row must carry the first row's column names in
  order; a column's dtype is pinned by the first non-`Null` declaration; a
  conflict aborts the run naming the column. A row carrying a column named like
  the sink's time column aborts too. Rows sharing one `Arc<Schema>` skip the
  check by pointer equality — build the schema once.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/polars_adapter.rs` | `#![cfg(feature = "polars")]` | nothing |

```bash
cargo test -p wingfoil --features polars --test polars_adapter
```

No integration tier and no dedicated workflow (skill step 10, Option C — the
file round trips *are* the integration test). Runs in `rust-test.yml`'s `test`
job.

## Example

`examples/adapters/polars/` (target `polars_adapter`),
`required-features = ["polars"]`.

## Python

`wingfoil-python` feature `polars = ["wingfoil/polars", "_common"]` — pure
Rust, **in `all-adapters` and in the wheel**. The binding names polars types
through the engine's re-exports; it has no polars dependency of its own.

- Entry points: `polars_read(graph, path, time_column, format=None,
  buffer_size=None)` and `polars_write(stream, path, time_column="time",
  format=None)`, both `#[pyadapter]`, in `src/adapters/polars.rs`.
- **Not bound:** an in-memory `DataFrame` in either direction, and so
  `polars_collect` — crossing a Python `polars.DataFrame` needs `pyo3-polars`,
  which pins its own pyo3/polars versions. Files are the interchange.
- Tests: `tests/test_polars.py`, **no marker** — runs by default in
  `python-test.yml`, round trips included (write with the binding, read back
  with the binding; no Python `polars` package needed). Rust marshaling tests
  in the binding's `mod tests` run there too via `--features all-adapters`.

```bash
cd crates/wingfoil-python && maturin develop -F extension-module,polars && pytest -q tests/test_polars.py
```

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil --features polars
```
