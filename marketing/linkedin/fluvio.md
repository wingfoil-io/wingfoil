# LinkedIn — Fluvio adapter

## Post

We recently shipped a Fluvio adapter for Wingfoil, so a quick overview.

Fluvio is a Kafka-compatible distributed streaming platform built in Rust. The integration follows the same shape as our Kafka adapter: `fluvio_sub` consumes from a topic partition and emits events into the graph; `fluvio_pub` (or the fluent `.fluvio_pub()` on a stream) writes a burst of records out to a topic. Records carry an optional key, and the offset is preserved on every consumed event, so you can hand `fluvio_sub` a start-from offset and resume from a known position rather than always replaying from the beginning.

One architectural note worth mentioning: Fluvio separates the cluster into a System Controller (SC, handles metadata, port 9003) and Stream Processing Units (SPUs, handle storage and serving). For local dev you just run `fluvio cluster start --local`, but knowing the two-process shape helps when you're debugging connectivity — the SC and an SPU both need to be reachable.

Writes are batched per burst rather than per record, same reasoning as the Kafka adapter: hand the whole burst to the producer and flush once at the end.

Available in Rust and via the PyO3 Python bindings.

## First comment

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/fluvio
- Example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/fluvio/main.rs
- Fluvio: https://github.com/infinyon/fluvio
