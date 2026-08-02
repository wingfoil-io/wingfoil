# ZeroMQ Adapter Example (wingfoil-next)

ZeroMQ pub/sub: publish a counter, subscribe to it, print it. A self-contained
port of the classic `wingfoil/examples/zmq` (direct mode) onto the next engine.

Two roles run as separate graphs in one process:

| Role | What it does |
|---|---|
| **publisher** | on a background thread, binds a `PUB` socket and publishes a UTF-8 counter every 100 ms |
| **subscriber** | in `main`, connects a `SUB` socket to that address and prints each value, plus connection-status transitions |

ZeroMQ is peer-to-peer — **no broker process is required**, which is what makes
this example self-contained where [`kafka`](../kafka/) and [`redis`](../redis/)
need a server.

## Run

```sh
cargo run -p wingfoil-next --example zmq_adapter --features zmq
```

## The slow-joiner problem

ZeroMQ `PUB` sockets drop messages sent before a subscriber's `SUBSCRIBE` has
propagated — the classic "slow joiner". A naive publisher that starts counting
immediately loses its first values, non-deterministically.

The publisher here buffers early messages until the subscription lands, so no
counter values are dropped at startup. That is why the output begins at 1 rather
than at some arbitrary number.

## Code

```rust
// Publisher, on its own thread: a counter every 100 ms.
let _pub = g
    .ticker(Duration::from_millis(100))
    .count()
    .map(|n: &u64| format!("{n}").into_bytes())
    .zmq_pub(PORT, ());

// Subscriber: connect, print each payload and each status transition.
let (data, status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, ADDRESS)?;

let _print = data.collapse().for_each(|msg: &Vec<u8>| {
    println!("received: {}", String::from_utf8_lossy(msg));
    Ok(())
});
let _status = status.for_each(|s: &ZmqStatus| {
    println!("status: {s:?}");
    Ok(())
});
```

`zmq_sub` returns **two** streams — data and status. Connection
state is a first-class stream rather than a callback, so reconnects and drops are
observable in the graph like any other event, and can be folded, filtered, or fed
into a circuit breaker. (See [`aeron`](../aeron/) for an example that does exactly
that.)

## Service discovery

This example hard-codes the address. The adapter also accepts an
`EtcdRegistry` — `("name", registry)` in place of a literal address — so peers can
find each other by name instead. That path is exercised by the
`zmq-etcd-integration-test` suite rather than by this example; see
[`etcd`](../etcd/) for the registry's backing store.

## Output

```text
status: Connected
received: 1
received: 2
received: 3
...
```

## See also

- [`aeron`](../aeron/) — brokerless again, but tuned for low-latency UDP/IPC.
- [`iceoryx2`](../iceoryx2/) — zero-copy shared memory for same-host peers.
- [`etcd`](../etcd/) — the registry behind ZeroMQ service discovery.
