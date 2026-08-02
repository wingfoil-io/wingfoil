# Redis Adapter Example (wingfoil-next)

Redis Pub/Sub end to end: publish, subscribe, transform, republish. A port of the
classic `wingfoil/examples/redis` example onto the next engine.

Redis Pub/Sub is fire-and-forget — a subscriber only sees messages published
*after* its `SUBSCRIBE` completes. The example therefore wires three roles as
separate graphs, each on its own thread with its own tokio runtime, and starts the
subscribers before the publisher sends anything:

| Role | What it does |
|---|---|
| **processor** | subscribes to `source`, uppercases each payload, republishes to `dest` |
| **verifier** | subscribes to `dest` and prints what it receives |
| **publisher** | publishes two messages to `source` |

## Prerequisites

A running Redis:

```sh
docker run --rm -p 6379:6379 redis:7-alpine
```

## Run

```sh
cargo run -p wingfoil-next --example redis_adapter --features redis
```

## Code

```rust
const URL:    &str = "redis://127.0.0.1:6379";
const SOURCE: &str = "example-source";
const DEST:   &str = "example-dest";

// processor: source -> uppercase -> dest
let g = GraphBuilder::new();
let _sink = redis_sub(&g, realtime(2).run_mode, conn.clone(), SOURCE)?
    .map(|burst: &Burst<RedisEvent>| {
        burst.iter()
            .map(|event| RedisEntry {
                channel: DEST.to_string(),
                payload: event.payload_str().unwrap_or("").to_uppercase().into_bytes(),
            })
            .collect::<Burst<RedisEntry>>()
    })
    .redis_pub(conn, None)?;
```

Two conventions worth noting, shared by every async adapter in this tree:

- **The graph owns the tokio runtime**, created lazily — no `&Handle` is threaded
  through the wiring. (Pass one explicitly with `with_async_runtime(..)` when you
  need to control its lifetime; see [`etcd`](../etcd/).)
- **Each graph is built, run, and dropped from a non-async thread.** The sinks
  drive writes with `Handle::block_on`, so `main` must not itself be async.

## Output

The verifier prints each message that lands on `dest`, as `channel -> payload`:

```text
  example-dest -> HELLO
  example-dest -> WORLD
```

The payloads went in as `hello` and `world` on `example-source`; the processor
uppercased them and republished to `example-dest`.

## See also

- [`etcd`](../etcd/) — the same sub → transform → pub shape over a watched key prefix.
- [`kafka`](../kafka/) — the durable-log equivalent.
- [`zmq`](../zmq/) — brokerless pub/sub.
