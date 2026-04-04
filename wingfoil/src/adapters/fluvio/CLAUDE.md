# Fluvio Adapter

Streams records from Fluvio topics (`fluvio_sub`) and writes records to Fluvio
topics (`fluvio_pub`). Fluvio is a distributed stream processing platform similar
to Kafka, using topics and partitions as its core abstractions.

## Module Structure

```
fluvio/
  mod.rs               # FluvioConnection, FluvioRecord, FluvioEvent, public re-exports
  read.rs              # fluvio_sub() producer
  write.rs             # fluvio_pub() consumer, FluvioPubOperators trait
  integration_tests.rs # Integration tests (requires Docker, gated by feature)
  CLAUDE.md            # This file
```

## Key Components

### Reading from Fluvio — `fluvio_sub`

- `fluvio_sub(conn, topic, partition, start_offset)` — produces `Burst<FluvioEvent>`
  - Streams records from a single partition starting at the given offset
  - `start_offset: None` → `Offset::beginning()` (reads all records from start)
  - `start_offset: Some(n)` → `Offset::absolute(n)` (skip records before offset `n`)
  - Each record yields a burst of exactly one `FluvioEvent`
  - Unlike etcd, there is **no snapshot + watch duality** — Fluvio is a pure stream

### Writing to Fluvio — `fluvio_pub`

- `fluvio_pub(conn, topic, upstream)` — consumes `Burst<FluvioRecord>`, sends one record per entry
- `FluvioPubOperators::fluvio_pub(conn, topic)` — fluent API on `Rc<dyn Stream<Burst<FluvioRecord>>>`
- Records are flushed **once per burst** (after all records in a burst are sent) for batching efficiency
- `FluvioRecord::new(value)` — keyless record (`RecordKey::NULL`)
- `FluvioRecord::with_key(key, value)` — record with a string key

### Types

- `FluvioConnection::new(endpoint)` — single SC endpoint in `"host:port"` format, e.g. `"127.0.0.1:9003"`
- `FluvioRecord { key: Option<String>, value: Vec<u8> }` — record to write
- `FluvioEvent { key: Option<Vec<u8>>, value: Vec<u8>, offset: i64 }` — record received

## Fluvio Cluster Setup

Fluvio is **not** a single-process server. A minimum cluster has:
- **SC** (System Controller) — metadata and coordination, default port **9003**
- **SPU** (Stream Processing Unit) — stores and serves records, default port **9010**

For local development, use the Fluvio CLI to start a cluster:

```sh
fluvio cluster start --local
```

Or run the SC in Docker (note: SPU must also be configured separately):

```sh
docker run --rm -d -p 9003:9003 -p 9010:9010 \
  infinyon/fluvio:0.18.1 \
  fluvio-run sc --local /tmp/fluvio
```

Topics must be **pre-created** before any producer or consumer connects:

```sh
fluvio topic create my-topic
# Or programmatically via FluvioAdmin:
admin.create::<TopicSpec>("my-topic", false, TopicSpec::new_computed(1, 1, None)).await?
```

## Integration Test Details

Tests use `testcontainers` (`SyncRunner`) to start an `infinyon/fluvio:0.18.1`
container running the SC in local mode. Docker must be running.

Feature flag: `fluvio-integration-test` (implies `fluvio`).

Tests must be run with `--test-threads=1` to avoid port conflicts.

### Known gotchas

1. **Container wait time**: The integration tests use `WaitFor::millis(8_000)` for
   container startup. If tests fail due to the SC not being ready in time, increase
   this value. The exact log message emitted by the SC on startup is
   implementation-dependent and may vary across Fluvio versions.

2. **SPU not registered**: The SC container alone does not automatically start an SPU.
   Producer/consumer operations require a registered SPU. If tests fail with "no SPU
   registered", the container setup needs to include SPU startup. Consider using
   `fluvio cluster start --local` inside the container to start both SC and SPU.

3. **Topic must exist**: Fluvio will return an error (`TopicNotFound`) if you try to
   produce to or consume from a topic that does not exist. Always call `create_topic`
   before producer/consumer tests.

4. **`Offset::absolute(n)` returns `Result`**: Unlike `Offset::beginning()`, this
   constructor validates the offset and fails for negative values.

5. **`ErrorCode` formatting**: `fluvio_protocol::link::ErrorCode` does not implement
   `std::error::Error`, so use `{:?}` (Debug) rather than `{}` (Display) in error
   messages.

6. **fluvio crate version**: Pinned to `0.50.1`. The `fluvio` crate on crates.io
   follows independent versioning from the Fluvio platform — `0.50.1` corresponds to
   platform version `0.18.x`. Do not use `default-features = false` without verifying
   the TLS configuration compiles correctly for the target platform.

## Pre-Commit Requirements

1. **Run integration tests (requires Docker + running Fluvio cluster):**

   ```bash
   cargo test --features fluvio-integration-test -p wingfoil \
     -- --test-threads=1 fluvio::integration_tests
   ```

2. **Run standard checks:**

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features
   cargo test -p wingfoil
   ```

### Test Coverage

| Test | What it proves |
|------|----------------|
| `test_connection_refused` | Error propagates when Fluvio SC is unreachable |
| `test_sub_from_beginning` | Pre-seeded records appear when consuming from offset 0 |
| `test_pub_round_trip` | `fluvio_pub` writes are readable via direct consumer |
| `test_sub_live_stream` | Records produced after consumer starts are received |
| `test_pub_keyless_record` | `RecordKey::NULL` (key-less) records work correctly |
| `test_pub_keyed_record` | String keys are stored and readable |
| `test_sub_from_absolute_offset` | `start_offset: Some(n)` skips earlier records |
