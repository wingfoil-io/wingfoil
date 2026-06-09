# cache (KDB+ result cache)

**This is not a standalone I/O adapter.** It is an internal support module for the
KDB+ adapter and is intentionally exempt from most of the new-adapter checklist
(no feature flag of its own, no graph nodes, no example, no Python bindings, no
CI workflow). It lives in its own directory only to keep the file-cache logic
separate from the kdb read/write code.

## What it is

A disk-backed result cache used exclusively by
[`kdb_read_cached`](../kdb/read_cached.rs). When a backtest replays the same
time-sliced query repeatedly, the rows fetched for each slice are cached on disk
so subsequent runs skip the round-trip to the database.

It is gated under the **`kdb`** feature (see `adapters/mod.rs`:
`#[cfg(feature = "kdb")] pub mod cache;`) — there is no separate `cache` feature.

## Module Structure

```
cache/
  mod.rs         # CacheKey (SHA-256 of host/port/query), CacheConfig (folder + LRU cap)
  file_cache.rs  # FileCache<T> — async get/put of serialized rows, LRU eviction
  CLAUDE.md      # This file
```

## Key Design Decisions

- **Cache key** — `CacheKey::from_parts([host, port, query])` is the first 64 bits
  of a SHA-256 digest, with `\0` separators so `["ab","c"]` ≠ `["a","bc"]`.
  SHA-256 (not `DefaultHasher`) keeps keys stable across toolchain versions; a
  unit test pins the exact hex prefix so an accidental algorithm change is caught.
- **File format** — each `<hex>.cache` file starts with the query string and a
  `\n`, then the bincode-encoded `Vec<(u64 nanos, T)>`. The header makes files
  self-documenting (`head -1 <hex>.cache` shows the producing query).
- **LRU eviction** — `CacheConfig { folder, max_size_bytes }`. On `put`, if the
  total on-disk size of `*.cache` files would exceed `max_size_bytes`, the oldest
  files (by mtime) are deleted first. A `get` rewrites the file to bump its mtime
  so a cache hit counts as "recently used". Set `max_size_bytes` to `u64::MAX`
  for an unbounded cache.
- **Atomic writes** — `put` writes to `<hex>.tmp` then `rename`s, so a `.cache`
  file is never left torn. Concurrent writers for the same key share the `.tmp`
  path and may clobber each other's serialization work, but the rename stays
  atomic — harmless for the intended single-process backtesting use case.
- **No locks on the graph thread** — all I/O is `tokio::fs` inside `async`
  `get`/`put`, driven off-graph by kdb's `produce_async`. There is no `cycle()`.

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test -p wingfoil --features kdb -- adapters::cache
```

The cache is also exercised end-to-end by the kdb cached-read integration tests
(`cargo test --features kdb-integration-test -p wingfoil -- kdb::cache`).
