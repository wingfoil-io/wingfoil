# Kafka Adapter

Streams messages from Kafka topics (`kafka_sub`) and produces messages to
Kafka topics (`kafka_pub`).

## Module Structure

```
kafka/
  mod.rs               # KafkaConnection, KafkaRecord, KafkaEvent, public re-exports
  read.rs              # kafka_sub() producer
  write.rs             # kafka_pub() consumer, KafkaPubOperators trait
  integration_tests.rs # Integration tests (requires Docker, gated by feature)
  CLAUDE.md            # This file
```

## Key Components

### Reading from Kafka — `kafka_sub`

- `kafka_sub(conn, topic, group_id)` — produces `Burst<KafkaEvent>`
  - Creates a `StreamConsumer` in the given consumer group
  - Subscribes to the topic and streams messages as they arrive
  - Each message becomes a `KafkaEvent` with topic, partition, offset, key, and value
  - Uses `auto.offset.reset = earliest` to read from the beginning for new groups

### Writing to Kafka — `kafka_pub`

- `kafka_pub(conn, upstream)` — consumes `Burst<KafkaRecord>`, produces every record in the burst concurrently
- `KafkaPubOperators::kafka_pub(conn)` — fluent API on `Rc<dyn Stream<Burst<KafkaRecord>>>`
- Each `KafkaRecord` specifies its target topic, allowing multi-topic writes from a single consumer
- All records in a burst are handed to the producer up front and their delivery futures drained
  together via `FuturesUnordered`, so per-burst latency is one broker roundtrip rather than N
- Uses `FutureProducer` with delivery confirmation (5s per send)
- Calls `producer.flush(5s)` on upstream end so queued retries drain before the task exits

### Types

- `KafkaConnection::new(brokers)` — broker string (e.g. `"localhost:9092"`)
- `KafkaRecord { topic, key: Option<Vec<u8>>, value: Vec<u8> }` — message to produce
- `KafkaEvent { topic, partition, offset, key, value }` — consumed message

## Dependencies

- `rdkafka` using its default build path (`./configure` + `make` for the bundled librdkafka).
  The `cmake-build` feature is **not** enabled, because its curl.h dependency breaks CI.
- Redpanda for integration tests — Kafka-compatible, fast startup

## Pre-Commit Requirements

1. **Run integration tests (requires Docker):**

   ```bash
   cargo test --features kafka-integration-test -p wingfoil \
     -- --test-threads=1 kafka::integration_tests
   ```

2. **Run standard checks:**

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features
   cargo test -p wingfoil
   ```

## Integration Test Details

Tests use `testcontainers` (`SyncRunner`) to start a
`docker.redpanda.com/redpandadata/redpanda:v24.1.1` container per test.
Docker must be running.

Feature flag: `kafka-integration-test` (implies `kafka`).

Tests must be run with `--test-threads=1`. Each container picks a fresh OS-assigned
host port via `free_port()` so parallel developers (or a local broker on the standard
9092) do not collide, but only one test at a time grabs a port — running multiple in
parallel races on the "bind → drop → container start" window.

### Advertise-address gotcha

Redpanda's `--advertise-kafka-addr` is the address clients are told to reconnect to
after the initial bootstrap. It must match what the client can actually reach, so the
container's internal listen port is identical to the host port (1:1 port mapping via
`with_mapped_port(port, port)`). This is why we can't simply rely on testcontainers'
dynamic port mapping — the advertised port has to be known before the container starts.

### Test coverage

| Test | What it proves |
|------|----------------|
| `test_connection_refused` | Handles unreachable broker gracefully |
| `test_sub_receives_pre_seeded_messages` | Pre-produced messages are consumed correctly |
| `test_sub_live_messages` | Live messages arrive during consumption |
| `test_pub_round_trip` | `kafka_pub` writes are readable via direct consumer |
| `test_pub_multiple_records_in_burst` | Multiple records in a single burst are all produced |
| `test_sub_event_fields` | All `KafkaEvent` fields are populated correctly |
| `test_kafka_record_value_str` | UTF-8 value interpretation works |
| `test_kafka_event_no_key` | Events without keys handled correctly |

## Notes

- `rdkafka` bundles `librdkafka` via its default build (`./configure` + `make`). The
  `cmake-build` feature flag is deliberately **not** used — it pulls in a curl.h
  dependency that breaks CI.
- Redpanda is used for tests instead of Apache Kafka because it starts much faster
  and is fully Kafka-protocol compatible.
- `kafka_sub` is designed for `RunMode::RealTime`. Using it in `HistoricalFrom` mode is
  technically valid but timestamps will be wall-clock `NanoTime::now()`, not historical.

## Python bindings

- Integration tests live in `wingfoil-python/tests/test_kafka.py`.
- Gated by the `requires_kafka` pytest marker, registered in
  `wingfoil-python/pyproject.toml` and deselected by default — a plain
  `pytest` run will not collect them.
- The Python integration workflow opts in explicitly:
  `pytest -m requires_kafka tests/test_kafka.py`. If the broker is not up
  the tests fail loudly with a real connection error rather than silently
  skipping (so CI can never be false-green).
