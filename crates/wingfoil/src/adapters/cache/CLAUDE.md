# cache Adapter (wingfoil)

A file-backed, query-keyed, LRU-evicting result cache for time-sliced
historical readers. Ports legacy `wingfoil::adapters::cache`.

**Not a source or sink.** There is no graph edge here: it is a plain utility
with async `get`/`put`, called by a reader from its own producer path.
[`kdb_read_cached`](../kdb/CLAUDE.md) is its only consumer today.

> Legacy has the `cache` module but never had a directory `CLAUDE.md` for it.
> This one is new; the parity oracle is `legacy/wingfoil/src/adapters/cache/`.

## Layout

```
adapters/
  cache.rs          # CacheKey, CacheConfig, FileCache
  cache/CLAUDE.md   # this file
```

## Feature gating

```toml
cache = ["dep:sha2", "dep:bincode", "dep:serde", "async", "tokio/fs"]
```

`kdb = [..., "cache"]` — the kdb feature pulls it in, and with it
`sha2`/`bincode`/`serde`/`tokio/fs`.

## Entry points

`use wingfoil::adapters::cache::{CacheConfig, CacheKey, FileCache};`

| Item | Notes |
|---|---|
| `CacheKey::from_parts(&[&str])` | opaque, stable SHA-256 key (kdb uses `[host, port, query]`) |
| `CacheConfig::new(folder, max_size_bytes)` | `u64::MAX` for unbounded |
| `CacheConfig::clear()` | deletes every `.cache` file in the folder |
| `FileCache::<T>::new(config)` | the store |
| `FileCache::get(&key) -> Result<Option<Vec<(NanoTime, T)>>>` | async; `T: Deserialize` |
| `FileCache::put(&key, query, &[(NanoTime, T)])` | async; `T: Serialize` |

## What to know before changing it

- **`bincode` is not self-describing.** Change `T`'s shape and old cache files
  decode into garbage or fail. The contract is: call `CacheConfig::clear()` (or
  delete the directory) on a schema change. Say so wherever a new consumer
  lands.
- **A corrupt cache file is a warning, not an error** — it logs, falls back to
  the live query, and overwrites the bad file. Keep that; a poisoned cache must
  not abort a backtest.
- **LRU eviction is by file mtime**, applied so total on-disk size stays under
  `max_size_bytes`.
- **The key does not encode the run window.** It is derived from the query
  string, so the cache stores the *full* slice a query returned. Any window
  clamping belongs in the caller, on emit, on hits **and** misses — see how
  `kdb_read_cached` applies `WindowFilter` from `adapters::common`. Do not
  push clamping into the cache.
- **A full-hit replay never opens a socket.** `kdb_read_cached` connects
  lazily; if every slice hits, no TCP connection is made. Preserve that when
  touching the `get` path.
- File I/O is tokio's `fs` — this module is async, so its callers already need
  a runtime (the graph's).

## Deviations from legacy

One, cosmetic: `FileCache`'s log messages drop legacy's `"KDB "` prefix — the
cache is not kdb-specific in wingfoil (register **D7**). Every public capability is
otherwise preserved, which is why the legacy unit tests port verbatim.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/cache_adapter.rs` | `#![cfg(feature = "cache")]` | nothing |

Ported verbatim from legacy `wingfoil::adapters::cache` (`mod.rs` +
`file_cache.rs` test modules): key stability/uniqueness, `CacheConfig::clear`,
and the store round-trip + eviction.

```bash
cargo test -p wingfoil --features cache --test cache_adapter
```

The cache's behaviour *in a reader* is covered by the kdb tier:
`tests/kdb_integration.rs` (`kdb-integration-test`) ports legacy's
`cache_integration_tests.rs`.

No dedicated workflow. Runs in `rust-test.yml`'s `test` job.

## Example

None of its own — see `examples/kdb/read_cached/main.rs`
(`required-features = ["kdb"]`).

## Python

**No binding.** The Python `kdb_read` surface does not expose the cached
variant; `wingfoil-python` has no `cache` feature.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil --features cache
```
