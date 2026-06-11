# LinkedIn — Kafka adapter

## Post

Wingfoil's Kafka adapter has been in place for a while and we use it daily, so a quick walk-through.

Two nodes, same shape as our other adapters. `kafka_sub(conn, topic, group)` consumes from a topic and emits `KafkaEvent` values with the usual partition / offset / key / value bits attached. `kafka_pub` (or the fluent `.kafka_pub(conn)` on a stream) takes a burst of `KafkaRecord`s and produces them. Records carry their target topic, so a single stream can fan out to many topics if that's what you want.

The detail we cared most about getting right was per-burst latency on the publish side. Naively, producing N records sequentially gives you N broker roundtrips. We hand the whole burst to the producer up front and drain the delivery futures together with `FuturesUnordered`, so a burst of 50 records costs about one roundtrip, not 50.

Under the hood it's `rdkafka` on its default build path. Tests run against Redpanda in a testcontainer because it starts in a second or two and speaks the Kafka protocol. Python bindings ship alongside, with a `requires_kafka` pytest marker so the integration suite fails loudly rather than silently skipping on a misconfigured runner.

Nothing exotic — just a Kafka client that fits the rest of the graph.

## First comment

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/kafka
- Round-trip example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/kafka/main.rs
- rdkafka: https://github.com/fede1024/rust-rdkafka
- Redpanda: https://github.com/redpanda-data/redpanda
