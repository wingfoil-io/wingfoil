# Redis Adapter

Two transports:

- **Pub/Sub** — subscribe to a channel (`redis_sub`) and publish messages (`redis_pub`)
- **Streams** — snapshot+tail read of a stream (`redis_stream_read`) and append (`redis_stream_write`)

`HSET`/key-value operations are intentionally out of scope.

## Module Structure

```
redis/
  mod.rs               # RedisConnection + Pub/Sub types (RedisEntry, RedisEvent)
                       #   + Stream types (RedisStreamRecord, RedisStreamEvent), re-exports
  read.rs              # redis_sub() producer (SUBSCRIBE)
  write.rs             # redis_pub() consumer (PUBLISH), RedisPubOperators trait
  stream.rs            # redis_stream_read() + redis_stream_write(), RedisStreamOperators trait
  integration_tests.rs # Integration tests (requires Docker, gated by feature)
  CLAUDE.md            # This file
```

## Key Components

### Reading from Redis — `redis_sub`

- `redis_sub(conn, channel)` — produces `Burst<RedisEvent>`
  - Connects and issues `SUBSCRIBE channel` once at startup
  - Emits each incoming message as a `RedisEvent { channel, payload }`
  - Built on `produce_async`: the async pub/sub stream runs on the tokio runtime
    and marshals messages into the graph

### Writing to Redis — `redis_pub`

- `redis_pub(conn, upstream)` — consumes `Burst<RedisEntry>`, issues one `PUBLISH`
  per entry to the channel the entry names
- `RedisPubOperators::redis_pub(conn)` — fluent API on both
  `Rc<dyn Stream<Burst<RedisEntry>>>` and `Rc<dyn Stream<RedisEntry>>`
- Built on `consume_async`: connects once via a multiplexed async connection

### Reading from a stream — `redis_stream_read`

- `redis_stream_read(conn, key)` — produces `Burst<RedisStreamEvent>`
  - **Phase 1 (snapshot):** `XRANGE key - +` replays all existing entries; the last
    entry ID is captured (or `"0"` if the stream is empty)
  - **Phase 2 (tail):** `XREAD BLOCK 0 STREAMS key <last_id>` returns only entries with
    an ID strictly greater than `last_id`, so the snapshot→tail handoff misses nothing
    and never duplicates

### Writing to a stream — `redis_stream_write`

- `redis_stream_write(conn, upstream)` — consumes `Burst<RedisStreamRecord>`, issues one
  `XADD key *` per record (Redis assigns the entry ID)
- `RedisStreamOperators::redis_stream_write(conn)` — fluent API on both
  `Rc<dyn Stream<Burst<RedisStreamRecord>>>` and `Rc<dyn Stream<RedisStreamRecord>>`

### Types

- `RedisConnection::new(url)` — Redis URL (e.g. `"redis://127.0.0.1:6379"`);
  `From<&str>` / `From<String>` so factories accept `impl Into<RedisConnection>`
- `RedisEntry { channel, payload }` — Pub/Sub message to publish; `.payload_str()` for UTF-8
- `RedisEvent { channel, payload }` — received Pub/Sub message; `.payload_str()` for UTF-8
- `RedisStreamRecord { key, fields: Vec<(String, Vec<u8>)> }` — entry to `XADD`;
  `::single(key, field, value)` for the common one-field case
- `RedisStreamEvent { key, id, fields }` — entry read from a stream; `.field(name)` lookup

## Pub/Sub: fire-and-forget (no snapshot)

Redis Pub/Sub has **no backlog, replay, or offsets**. A subscriber only receives
messages published *after* its `SUBSCRIBE` completes, so there is no snapshot phase.

The Pub/Sub integration tests order subscribe-before-publish explicitly: the publisher
either retries until the graph subscriber has captured a message, or waits for a `ready`
flag the subscriber sets only after `SUBSCRIBE` returns.

## Streams: snapshot → tail handoff (race prevention)

```
XRANGE key - +  ──→ capture last_id ──→ emit snapshot ──→ XREAD BLOCK STREAMS key last_id
                                                            (only ids > last_id)
```

Because the tail reads from the exact snapshot boundary ID, any entry appended between
the `XRANGE` and the first `XREAD` is returned by `XREAD` (its ID is greater) — never
missed — and no snapshot entry is re-delivered. This mirrors `etcd_sub`'s
watch-before-GET guarantee, but with stream IDs instead of etcd revisions. Streams
persist, so unlike Pub/Sub the integration tests need no subscribe-before-write ordering.

## Pre-Commit Requirements

1. **Run integration tests (requires Docker):**

   ```bash
   cargo test --features redis-integration-test -p wingfoil \
     -- --test-threads=1 redis::integration_tests
   ```

2. **Run standard checks:**

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features
   cargo test -p wingfoil
   ```

## Integration Test Details

Tests use `testcontainers` (`SyncRunner`) to start a `redis:7-alpine` container per
test. Docker must be running. No environment variables required.

Feature flag: `redis-integration-test` (implies `redis`).

Tests must be run with `--test-threads=1` to avoid port conflicts between containers.

### Test coverage

| Test | What it proves |
|------|----------------|
| `test_connection_refused` | Error propagates when Redis is unreachable |
| `test_sub_live_message` | A message published while subscribed arrives as a `RedisEvent` |
| `test_pub_round_trip` | `redis_pub` writes are received by a direct subscriber |
| `test_pub_multiple_entries_in_burst` | All entries in a single burst are published |
| `test_redis_entry_payload_str` | UTF-8 payload interpretation works |
| `test_stream_write_and_read_snapshot` | `redis_stream_write` entries replay via the snapshot phase |
| `test_stream_read_live_tail` | An entry appended after the snapshot arrives via the `XREAD` tail |
| `test_stream_record_single` | `RedisStreamRecord::single` builds the expected record |

## Notes

- `redis` is pinned to `"0.27"` with the `tokio-comp` + `aio` + `streams` features.
- `get_async_pubsub()` (async PubSub), `get_multiplexed_async_connection()` (publishing /
  XADD / XREAD), and the typed `redis::streams` replies (`StreamRangeReply`,
  `StreamReadReply`) are all stable in 0.27.
- The stream tail uses `XREAD BLOCK 0` on a dedicated connection used only for reading,
  so blocking that connection indefinitely is safe.
- We use `GenericImage` with the official `redis:7-alpine` image directly rather than a
  `testcontainers-modules` helper, matching the etcd/kafka adapters.
- `redis_sub` is designed for `RunMode::RealTime`. Using it in `HistoricalFrom` mode is
  technically valid but timestamps will be wall-clock `NanoTime::now()`, not historical.

## Python bindings

- Binding module: `wingfoil-python/src/py_redis.rs` (`py_redis_sub` + `py_redis_pub_inner`).
- Integration tests live in `wingfoil-python/tests/test_redis.py`, gated by the
  `requires_redis` pytest marker (registered in `pyproject.toml` and deselected by
  default). Unit-level construction/marshaling tests in the same file run by default.
