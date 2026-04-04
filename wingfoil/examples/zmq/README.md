# ZMQ Examples

```
zmq/
  etcd/   — etcd-based service discovery (requires etcd)
```

Two modes are supported:

| Mode | When to use |
|------|-------------|
| **Direct** (`zmq_pub` / `zmq_sub` with a plain address) | Simple setups, fixed topology |
| **etcd** (`zmq_etcd_pub` / `zmq_etcd_sub`) | Dynamic topology, existing etcd infra, auto-expiry on crash |

## Direct pub/sub

Run in separate terminals:

```sh
RUST_LOG=info cargo run --example zmq_etcd_pub --features zmq-beta,etcd  # or use zmq_pub (no discovery)
RUST_LOG=info cargo run --example zmq_etcd_sub --features zmq-beta,etcd
```

For direct connections without discovery, pass a plain address to `zmq_sub`:

```rust
let (data, status) = zmq_sub::<Vec<u8>>("tcp://localhost:5556")?;
```

## etcd/

| Example | Description |
|---------|-------------|
| `zmq_etcd_pub` | Bind a PUB socket and register address in etcd under a lease |
| `zmq_etcd_sub` | Look up publisher address from etcd and subscribe |

Start etcd first:

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

Then run publisher and subscriber in separate terminals:

```sh
RUST_LOG=info cargo run --example zmq_etcd_pub --features zmq-beta,etcd
RUST_LOG=info cargo run --example zmq_etcd_sub --features zmq-beta,etcd
```

The publisher's address lease expires ~30 s after a crash, or immediately on clean shutdown.
