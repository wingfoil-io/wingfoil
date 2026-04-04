# CSV Adapter

Reads and writes comma-separated values files as wingfoil streams.

## Module Structure

```
csv/
  mod.rs        # Module-level doc, re-exports from read and write
  read.rs       # csv_read, csv_read_vec, csv_iterator (pub(super))
  write.rs      # CsvWriterNode, CsvVecWriterNode, CsvOperators, CsvVecOperators, tests
  test_data/    # CSV fixtures used by unit tests
  CLAUDE.md     # This file
```

## Key Components

### Reading — `csv_read` / `csv_read_vec`

- `csv_read(path, get_time_func, has_headers)` — emits one `T` per tick; source must be strictly ascending in time (uses `SimpleIteratorStream`)
- `csv_read_vec(path, get_time_func, has_headers)` — emits `Vec<T>` per tick; multiple rows with the same timestamp are grouped (uses `IteratorStream`)
- Both functions delegate to the private `csv_iterator` which deserialises rows via `serde`

### Writing — `CsvOperators` / `CsvVecOperators`

- `.csv_write(path)` — fluent method on `Rc<dyn Stream<T>>`; writes one row per tick with a leading `time` column
- `.csv_write_vec(path)` — fluent method on `Rc<dyn Stream<Vec<T>>>`; writes one row per element per tick

Headers are written lazily on first tick using `serde_aux::serde_introspection::serde_introspect`.

### `IteratorStream` / `SimpleIteratorStream`

These general-purpose nodes live in `crate::nodes::iterator_stream` (not the adapters layer).
The CSV adapter imports them directly; they are also available via `wingfoil::*`.

## Test Data

Unit tests live in `write.rs` and use files under `src/adapters/csv/test_data/`
(paths are relative to the package root `wingfoil/`, which is Cargo's cwd when running tests).

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test -p wingfoil --features csv
```
