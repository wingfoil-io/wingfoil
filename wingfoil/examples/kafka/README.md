# Kafka Adapter Example

Demonstrates a full round-trip with the Kafka adapter:

1. **Seed** — writes two messages to `example-source` via `kafka_pub`
2. **Round-trip** — consumes `example-source` with `kafka_sub`, uppercases each value, writes to `example-dest` via `kafka_pub`

## Setup

### Local (Docker)

Using Redpanda (Kafka-compatible, fast startup):

```sh
docker run --rm -p 9092:9092 \
  docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
  redpanda start --overprovisioned --smp 1 --memory 512M \
  --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
```

## Run

```sh
cargo run --example kafka --features kafka
```

## Code

See `main.rs` for the full source.

## Output

```
  greeting → HELLO
  subject → WORLD
```
