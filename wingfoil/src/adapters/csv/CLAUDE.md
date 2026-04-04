# CSV Adapter

Reads and writes comma-separated values files as wingfoil streams.

## Module Structure

```
csv/
  mod.rs        # Module-level doc, re-exports from read and write
  read.rs       # csv_read, csv_iterator (pub(super))
  write.rs      # CsvWriterNode, CsvOperators, tests
  test_data/    # CSV fixtures used by unit tests
  CLAUDE.md     # This file
```

## Key Components

### Reading — `csv_read`

- `csv_read(path, get_time_func, has_headers)` — emits `Burst<T>` per tick; multiple rows with the same timestamp are grouped into a single burst (uses `IteratorStream`)
- Delegates to the private `csv_iterator` which deserialises rows via `serde`

### Writing — `CsvOperators`

- `.csv_write(path)` — fluent method on both `Rc<dyn Stream<Burst<T>>>` and `Rc<dyn Stream<T>>`; writes one row per element per tick with a leading `time` column
- Single-value streams are auto-wrapped into a one-element burst

Headers are written lazily on first tick using `serde_aux::serde_introspection::serde_introspect`.

### `IteratorStream` / `SimpleIteratorStream`

These general-purpose nodes live in `crate::nodes` (not the adapters layer).
The CSV adapter imports them directly; they are also available via `wingfoil::*`.

## Test Data

Unit tests live in `write.rs` and use files under `src/adapters/csv/test_data/`
(paths are relative to the package root `wingfoil/`, which is Cargo's cwd when running tests).

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test -p wingfoil
```
