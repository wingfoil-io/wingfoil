# redis Adapter (wingfoil-next)

**Two transports, each with a source and a sink** — Pub/Sub
(`SUBSCRIBE`/`PUBLISH`) and Streams (`XRANGE`/`XREAD`/`XADD`). Ports legacy
`wingfoil::adapters::redis` onto the Op model.

`HSET`/key-value operations are intentionally out of scope, matching legacy.

## Layout

```
adapters/
  redis.rs          # both transports: connection/entry/event/record types, sources, sink traits
  redis/CLAUDE.md   # this file
```

## Feature gating

```toml
redis = ["dep:redis", "dep:async-stream", "async"]
redis-integration-test = ["redis", "dep:testcontainers"]
```

The `redis` crate is pinned with `tokio-comp`, `aio`, `streams` — mirroring
legacy.

## Entry points

| Item | Transport | Kind |
|---|---|---|
| `redis_sub(g, run_mode, conn, channel)` | Pub/Sub | source → `Stream<Burst<RedisEvent>>` |
| `RedisSinkOps::redis_pub(conn, buffer_size)` | Pub/Sub | sink on `Stream<Burst<RedisEntry>>` and `Stream<RedisEntry>` |
| `redis_stream_read(g, run_mode, conn, key)` | Streams | source → `Stream<Burst<RedisStreamEvent>>` |
| `RedisStreamSinkOps::redis_stream_write(conn, buffer_size)` | Streams | sink on `Stream<Burst<RedisStreamRecord>>` and `Stream<RedisStreamRecord>` |

Types: `RedisConnection` (+ `redacted()`), `RedisEntry`, `RedisEvent`,
`RedisStreamRecord` (`single(key, field, value)`), `RedisStreamEvent`
(`field(name)`).

## What to know before changing it

- **Both sources are realtime-only, rejected at wiring** (register B2,
  ratified). Pub/Sub has no backlog; the stream tail blocks forever. A
  historical run would block-collect the whole stream and deadlock at `start`.
- **Pub/Sub has no snapshot phase, ever.** A subscriber only receives messages
  published *after* its `SUBSCRIBE` completes. A test or example that publishes
  before subscribing will see nothing — order subscribe before publish, retry
  until captured, or gate on a ready flag.
- **The Streams snapshot→tail handoff is race-free by construction.**
  `XRANGE key - +` for the snapshot, capture its last entry ID, then
  `XREAD BLOCK 0 STREAMS key <last_id>` — which only returns IDs strictly
  greater. Anything appended between the two is returned by `XREAD` (never
  missed) and no snapshot entry is re-delivered. This mirrors `etcd_sub`'s
  watch-before-GET with stream IDs instead of revisions. Do not reorder.
- **The whole snapshot rides one atomic `Burst`** — every existing entry is
  stamped with a single shared `NanoTime::now()`, so it is never split across
  cycles or latest-wins'd. Legacy stamped each entry with its own
  `NanoTime::now()`; this is a deliberate burst-model change.
- **`RedisConnection::redacted()` must be used at every error site.** A
  `redis://user:pass@host` URL embeds credentials, and `.context("connecting
  to {conn}")` would spill them into logs and the graph-abort error. Four unit
  tests pin the masking (user+password, password-only userinfo, `rediss://`,
  and the no-userinfo no-op). This is the credential-redaction rule from
  `/new-adapter-next`.
- **The sinks connect lazily inside the `consume_async` consumer**
  (register A1/A4). `Client::open` — a pure URL parse — still validates at
  wiring, so a malformed URL is an `Err` before the run while a *connection*
  failure surfaces during it.
- `consume_async` ⇒ the `block_on` footgun (A5a): build, run and drop the graph
  from a **non-async** thread.
- Both sinks take a `buffer_size` for `consume_async` back-pressure
  (register D3); `None` is unbounded.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `redis.rs` — the
graph-owned runtime (A5), lazy sink connect (A1/A4), and the sink-as-trait fold
(D1, one trait per transport where legacy had free fns *and* operator traits);
plus the two burst-model notes above (single-burst snapshot, wiring-time
historical rejection). Every legacy capability (Pub/Sub sub+pub, Streams
snapshot→tail read + append, all four value types) is preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/redis_adapter.rs` | `#![cfg(feature = "redis")]` | nothing |
| `tests/redis_integration.rs` | `#![cfg(feature = "redis-integration-test")]` | a Redis container |

```bash
cargo test -p wingfoil-next --features redis --test redis_adapter
cargo test -p wingfoil-next --features redis-integration-test -- --test-threads=1
```

```sh
docker run --rm -p 6379:6379 redis:7-alpine
```

**Workflow:** `.github/workflows/redis-next-integration.yml` (in
`integration-tests.yml`), Rust leg + `pytest -m requires_redis` Python leg.

## Example

`examples/redis_adapter.rs`, `required-features = ["redis"]`.

## Python

`wingfoil-next-python` feature `redis = ["wingfoil-next/redis", "_common"]`.
**In `all-adapters` and in the wheel** (pure Rust).

- Entry points, all `#[pyadapter]` in `src/adapters/redis.rs`: `redis_sub`,
  `redis_pub`, `redis_stream_read`, `redis_stream_write` — the full four.
- Tests: `tests/test_redis.py` — service-free group by default,
  `@pytest.mark.requires_redis` group in the workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features redis
# with a container available:
cargo test -p wingfoil-next --features redis-integration-test -- --test-threads=1
```
