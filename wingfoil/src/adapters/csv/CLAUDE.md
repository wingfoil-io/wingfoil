# CSV Adapter

Reads and writes comma-separated values files as wingfoil streams.

## Module Structure

```
csv/
  mod.rs        # Module-level doc, re-exports from read and write
  read.rs       # csv_read, private csv_iterator, tests
  write.rs      # CsvWriterNode, CsvOperators, tests
  test_data/    # CSV fixtures used by unit tests
  CLAUDE.md     # This file
```

## Key Components

### Reading — `csv_read`

- `csv_read(path, get_time_func, has_headers)` — returns `anyhow::Result<Rc<dyn Stream<Burst<T>>>>` (a missing file is an error, not a panic); emits `Burst<T>` per tick; multiple rows with the same timestamp are grouped into a single burst (uses `TryIteratorStream`)
- Delegates to the private `csv_iterator` which deserialises rows via `serde`; a row that fails to deserialize surfaces as a graph-run error rather than a panic

### Writing — `CsvOperators`

- `.csv_write(path)` — fluent method on both `Rc<dyn Stream<Burst<T>>>` and `Rc<dyn Stream<T>>`; writes one row per element per tick with a leading `time` column
- Single-value streams are auto-wrapped into a one-element burst

Headers are written lazily on first tick using `serde_aux::serde_introspection::serde_introspect`.

### `TryIteratorStream` / `IteratorStream` / `SimpleIteratorStream`

These general-purpose nodes live in `crate::nodes` (not the adapters layer).
The CSV adapter imports `TryIteratorStream` directly (its iterator yields
`anyhow::Result` items so row errors propagate); they are also available via `wingfoil::*`.

## Test Data

Unit tests live in `read.rs` and `write.rs` and use files under `src/adapters/csv/test_data/`
(paths are relative to the package root `wingfoil/`, which is Cargo's cwd when running tests).

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features
cargo test -p wingfoil --features csv
```

The adapter is gated behind the (non-default) `csv` feature.
