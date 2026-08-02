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
- `FluvioPubOperators::fluvio_pub(conn, topic)` — fluent API, implemented for both
  `Rc<dyn Stream<Burst<FluvioRecord>>>` and `Rc<dyn Stream<FluvioRecord>>` (single records are auto-wrapped)
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
container with **host networking**, running a full local cluster (SC + SPU).
Docker must be running. `start_fluvio()` in `integration_tests.rs`:

1. Starts the SC and waits for the log line `"Streaming Controller started successfully"`,
   then probes port 9003 until it accepts (the log line races the listener bind).
2. Registers a `CustomSpuSpec` via `FluvioAdmin` — the SC must know about the SPU
   before the SPU process connects, otherwise the SC closes the connection.
3. Execs the SPU process (`/fluvio-run spu`) into the running container.
4. Sleeps 3 s for the SPU to complete registration.

Host networking is required so the SPU public endpoint (`127.0.0.1:9010`), which
the SC hands to clients, is reachable from the test process.

Feature flag: `fluvio-integration-test` (implies `fluvio`; also pulls in
`testcontainers` and `fluvio-controlplane-metadata`).

Tests must be run with `--test-threads=1` to avoid port conflicts.

### Known gotchas

1. **SC startup race**: the SC logs "started successfully" *before* binding its 9003
   listener; without the port probe, back-to-back tests fail with `ECONNREFUSED`.

2. **SPU must be pre-registered**: producer/consumer operations require a registered
   SPU. If tests fail with "no SPU registered", the SPU registration/exec sequence
   above did not complete — check the 3 s registration sleep is long enough.

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

1. **Run integration tests (requires Docker; tests start their own cluster):**

   ```bash
   cargo test --features fluvio-integration-test -p wingfoil \
     -- --test-threads=1 fluvio::integration_tests
   ```

2. **Run standard checks:**

   ```bash
   cargo fmt --all
   cargo lint        # default features
   cargo lint-all    # all features
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
