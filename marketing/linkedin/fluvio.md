# LinkedIn — Fluvio adapter

## Post

We recently shipped a Fluvio adapter for Wingfoil, so a quick overview.

Fluvio is a Kafka-compatible distributed streaming platform built in Rust. The integration follows the same shape as our Kafka adapter: `fluvio_sub` consumes from a topic partition and emits events into the graph; `fluvio_pub` (or the fluent `.fluvio_pub()`) writes a burst of records out to a topic. Records carry an optional key, and the offset is preserved on every consumed event so you can resume from an exact position rather than always starting from the beginning.

```rust
let conn = FluvioConnection::new("127.0.0.1:9003");

fluvio_sub(conn, "prices", 0, Some(1000))  // start from offset 1000
    .collapse()
    .for_each(|event, _| process(event));
```

One architectural note worth mentioning: Fluvio separates the cluster into a System Controller (SC, handles metadata, port 9003) and Stream Processing Units (SPUs, handle storage and serving). For local dev you just run `fluvio cluster start --local`, but knowing the two-process shape helps when you're debugging connectivity — the SC and an SPU both need to be reachable.

Writes are batched per burst rather than per record, same reasoning as the Kafka adapter: hand the whole burst to the producer and flush once at the end.

Available in Rust and via the PyO3 Python bindings.

## First comment

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/fluvio
- Example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/fluvio/main.rs
- Fluvio: https://github.com/infinyon/fluvio
