# Kafka Adapter Example (wingfoil)

Consume a Kafka topic, transform each record, and produce the result to another
topic. A port of the classic `wingfoil/examples/kafka` example onto the wingfoil
engine.

Two independent graph *roots* share one `GraphBuilder`:

| Root | What it does |
|---|---|
| **seed** | writes the source messages once, via `constant` + `kafka_pub` |
| **round_trip** | consumes the source topic, uppercases each value, produces to the destination topic |

## Prerequisites

A Kafka-compatible broker. Redpanda is the lightest way to get one:

```sh
docker run --rm -p 9092:9092 \
  docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
  redpanda start --overprovisioned --smp 1 --memory 512M \
  --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
```

The adapter is built on `rdkafka`, so it speaks to Apache Kafka, Redpanda, or
anything else wire-compatible.

## Run

```sh
cargo run -p wingfoil --example kafka_adapter --features kafka
```

## Code

```rust
let _round_trip = kafka_sub(&g, params.run_mode, BROKERS, SOURCE_TOPIC, "example-group")?
    .map(|burst: &Burst<KafkaEvent>| {
        burst.iter()
            .map(|event| KafkaRecord {
                topic: DEST_TOPIC.into(),
                key:   event.key.clone(),
                value: event.value_str().unwrap_or("").to_uppercase().into_bytes(),
            })
            .collect::<Burst<KafkaRecord>>()
    })
    .kafka_pub(BROKERS)?;
```

`kafka_sub` takes a consumer **group id** — the ordinary Kafka mechanism for
tracking offsets and sharing partitions between instances. Run two copies of this
example with the same group and they will split the partitions between them.

The record key is carried through unchanged so the transformed message keeps its
partition affinity.

## Output

The round-trip root prints each consumed key beside the value it produces:

```text
  greeting → HELLO
  subject → WORLD
```

## See also

- [`fluvio`](../fluvio/) — the same durable-log shape on a different broker.
- [`redis`](../redis/) — fire-and-forget Pub/Sub instead of a durable log.
- [`postgres`](../postgres/) — when you want the history queryable rather than replayed.
