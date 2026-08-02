# CSV Adapter (wingfoil-next)

A serde-typed CSV file adapter — a historical replay **source** and a file
**sink**. The parsing cousin of [`lines`](../lines/CLAUDE.md); ports classic
`wingfoil::adapters::csv` onto the Op model.

## Layout

```
adapters/
  csv.rs          # the whole adapter (source, sink trait, helpers)
  csv/CLAUDE.md   # this file
```

## Feature gating

```toml
csv = ["dep:csv", "dep:serde", "dep:serde-aux", "async", "dep:async-stream"]
```

Note the `async` implication — that is a **documented dependency gain over
classic** (register B4). It buys the lazy, bounded replay: the sink itself is
still synchronous.

## Entry points

| Item | Kind | Signature shape |
|---|---|---|
| `csv_read(g, path, get_time, has_headers, buffer_size)` | source | `Result<Stream<Burst<T>>>`, `T: DeserializeOwned` |
| `CsvSinkOps::csv_write(path)` | sink trait | on `Stream<Burst<T>>` **and** `Stream<T>` |
| `CsvSinkOps::csv_write_with_header(path, header)` | sink trait | explicit header, both impls |

Records are ordinary Rust types: a named struct, or a positional tuple such as
`(NanoTime, u32)`.

## What to know before changing it

- **`csv_read` is lazy and bounded** (register B4/B5). The file is opened at
  wiring (fail-fast, so a missing file is an `Err` before the run), but rows
  are deserialized **on demand** over a `produce_async` producer as the graph
  drains. `Some(n)` bounds look-ahead to ~`n` timestamp-groups in *both* run
  modes; `None` is unbounded. Do not move it back to `replay_results`.
- **Timestamps must be non-decreasing.** `get_time(&record)` feeds the
  monotonic graph clock; an out-of-order record aborts the run. This is a
  documented deviation — classic's `TryIteratorStream` imposed no explicit
  source-side ordering constraint.
- **Rows sharing a timestamp ride one atomic `Burst`.** Use
  `.collapse_accumulate()` when the source is strictly ascending and you want a
  flat `Vec<T>`; `.collapse()` keeps only the last row of a burst and will
  silently drop same-timestamp siblings.
- **The header is written eagerly, at wiring** — before `for_each_mut`. Classic
  deferred it to the first tick via a `headers_written` flag. Observable
  difference: a graph that wires `csv_write` and produces zero rows leaves a
  header-only file in next, an empty file in classic. Positional tuples have no
  named fields, so no header is written either way and there is no difference.
- The sink chains `with_time()` then `for_each_mut`, so every row carries a
  leading `time` column — same as classic.
- Both a `Stream<Burst<T>>` and a `Stream<T>` sink impl exist here (unlike
  `lines`): the bound is `Serialize`, which `Burst<T>` does not satisfy, so the
  two impls cannot collide.

## Deviations from classic

Canonical list: the `# Deviations from classic` block in `csv.rs`. In short —
non-decreasing timestamps required (above); eager header write (above);
malformed-row errors now surface **mid-stream** as the reader reaches the row
rather than at replay start (register D6 — the error string and run-failure
outcome are unchanged, and `csv_read` deliberately reuses classic's "failed to
deserialize row" context so classic's message assertions port verbatim).

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/csv_adapter.rs` | `#![cfg(feature = "csv")]` | nothing |

```bash
cargo test -p wingfoil-next --features csv --test csv_adapter
```

No integration tier and no dedicated workflow (skill step 10, Option C —
fixture files *are* the integration test). Runs in `rust-test.yml`'s
`test-next` job.

## Example

`examples/csv_adapter.rs`, `required-features = ["csv"]`.

## Python

`wingfoil-next-python` feature `csv = ["wingfoil-next/csv", "dep:csv", "_common"]`
— named `dep:csv` directly because the binding opens the file at wiring to read
its header. **In `all-adapters` and in the maturin wheel** (pure Rust,
dependency-light).

- Entry points: `csv_read(graph, …)`, `csv_write(stream, …)` — both
  `#[pyadapter]`-generated, in `src/adapters/csv.rs`.
- Tests: `tests/test_csv.py`, **no marker** — the whole file runs by default in
  `next-python-test.yml`. There is no `csv-next-integration.yml`, by design.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features csv
```
