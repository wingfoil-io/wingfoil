# etcd Adapter Example (wingfoil)

Watch an etcd key prefix, transform the values, and write them back under a
different prefix. A port of the classic `wingfoil/examples/etcd` example onto the
next engine.

Two independent graph *roots* share one `GraphBuilder`:

| Root | What it does |
|---|---|
| **seed** | writes the source keys once, via `constant` + `etcd_pub` |
| **round_trip** | watches the source prefix, uppercases each value, writes to the destination prefix |

A single graph can have several disconnected roots; the scheduler runs them all.

## Prerequisites

A running etcd instance:

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

## Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example etcd_adapter --features etcd
```

## Code

```rust
// This example builds its own runtime and installs it as the override, so it
// controls the runtime's lifetime. Omit `.with_async_runtime(..)` to let the
// graph create and own one lazily — that is the simpler, usual case.
let rt = tokio::runtime::Runtime::new()?;
let g  = GraphBuilder::new().with_async_runtime(rt.handle().clone());

// Root 1 — seed the source prefix with two keys, once.
let _seed = g
    .constant(burst![
        EtcdEntry { key: format!("{SOURCE_PREFIX}greeting"), value: b"hello".to_vec() },
        EtcdEntry { key: format!("{SOURCE_PREFIX}subject"),  value: b"world".to_vec() },
    ])
    .etcd_pub(conn.clone(), None, true)?;

// Root 2 — watch, transform, write back.
let _round_trip = etcd_sub(&g, params.run_mode, conn.clone(), SOURCE_PREFIX)?
    .map(|burst: &Burst<EtcdEvent>| {
        burst.iter()
            .map(|event| EtcdEntry {
                key:   event.entry.key.replacen(SOURCE_PREFIX, DEST_PREFIX, 1),
                value: /* uppercased */,
            })
            .collect::<Burst<EtcdEntry>>()
    })
    .etcd_pub(conn, None, false)?;
```

This is the one example in the tree that passes its own runtime handle — every
other async adapter lets the graph create one lazily. Both are supported; use the
override only when the runtime must outlive or predate the graph.

## Output

The round-trip root prints each watched key beside the value it writes on:

```text
  /example/source/greeting → HELLO
  /example/source/subject → WORLD
```

Those uppercased values land under `/example/dest/` — the same key with the
prefix swapped.

## See also

- [`redis`](../redis/) — the same sub → transform → pub shape over Pub/Sub channels.
- [`zmq`](../zmq/) — etcd also backs `EtcdRegistry` service discovery for ZeroMQ peers.
