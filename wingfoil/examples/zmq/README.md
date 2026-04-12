# ZMQ Examples

```
zmq/
  direct/  — hardcoded address, no discovery infra needed
  etcd/    — etcd-based service discovery (requires etcd)
```

Two modes are supported:

| Mode | When to use |
|------|-------------|
| **Direct** | Simple setups, fixed topology, no extra infra |
| **etcd** | Dynamic topology, existing etcd infra, auto-expiry on crash |

## direct/

Run publisher and subscriber in separate terminals:

```sh
RUST_LOG=info cargo run --example zmq_direct_pub --features zmq-beta
RUST_LOG=info cargo run --example zmq_direct_sub --features zmq-beta
```

| Example | Description |
|---------|-------------|
| `zmq_direct_pub` | Bind a PUB socket and publish a counter |
| `zmq_direct_sub` | Connect to a hardcoded address and subscribe |

## etcd/

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

| Example | Description |
|---------|-------------|
| `zmq_etcd_pub` | Bind a PUB socket and register address in etcd under a lease |
| `zmq_etcd_sub` | Look up publisher address from etcd and subscribe |

The publisher's address lease expires ~30 s after a crash, or immediately on clean shutdown.
