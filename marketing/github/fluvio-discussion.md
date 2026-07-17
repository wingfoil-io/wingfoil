# GitHub discussion — infinyon/fluvio

Suggested category: **Show and tell**

Suggested title: **Wingfoil — a Rust stream-processing graph with a Fluvio adapter**

---

## Body

Hi all,

We recently shipped a Fluvio adapter for Wingfoil and wanted to share it here, partly as a show-and-tell and partly because we ran into a few integration nuances that might be worth discussing.

### What Wingfoil is

Wingfoil is a Rust stream-processing library where you build a directed acyclic graph of nodes (map, filter, fold, custom transforms) that tick when their upstreams produce values. It runs in real-time or replays historical data deterministically. We use it for high-frequency financial data pipelines.

### The adapter

Two nodes:

- `fluvio_sub(conn, topic, partition, start_offset)` — consumes from a partition, emits `Burst<FluvioEvent>`. The `start_offset: Some(n)` path uses `Offset::absolute(n)`, so you can resume from a known position rather than always replaying from the beginning.
- `fluvio_pub` / `.fluvio_pub(conn, topic)` — writes a `Burst<FluvioRecord>` to a topic, flushing once per burst.

```rust
use wingfoil::adapters::fluvio::*;
use wingfoil::*;

let conn = FluvioConnection::new("127.0.0.1:9003");

// Produce
constant(burst![
    FluvioRecord::with_key("sym", b"AAPL".to_vec()),
    FluvioRecord::new(b"side=buy qty=100".to_vec()),
])
.fluvio_pub(conn.clone(), "orders");

// Consume and process in the same graph
fluvio_sub(conn, "orders", 0, None)
    .collapse()
    .for_each(|event, _| handle(event));
```

### Things we bumped into during integration

**SC + SPU separation.** We initially tried to test against just the SC container and hit "no SPU registered" errors. The adapter itself is fine — Fluvio just requires both processes to be running before any produce/consume can happen, and it wasn't immediately obvious from the error message that the missing piece was the SPU rather than a connection issue.

**Topic must be pre-created.** `TopicNotFound` is returned as an `ErrorCode` (not `std::error::Error`), so the debug message is less helpful than it could be at first glance. We now always call `create_topic` in test setup before touching the producer or consumer.

**Container startup timing.** We use testcontainers in integration tests with an 8-second wait for the SC, which is conservative but stable. Waiting on a specific log line would be tighter, but the exact message has shifted between Fluvio versions and we didn't want fragile tests.

### A question for the community

Is there a recommended pattern for starting a single-container local dev/test cluster (SC + SPU together) that's stable across `0.18.x` releases? We're currently using `fluvio cluster start --local` inside CI but a Docker-only path would simplify things.

### Links

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/fluvio
- Example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/fluvio/main.rs
- Wingfoil: https://github.com/wingfoil-io/wingfoil

Thanks for building Fluvio — happy to answer questions or share more detail on anything above.
