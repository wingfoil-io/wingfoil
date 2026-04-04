# ZMQ Examples

```
zmq/
  seed/   — direct pub/sub and seed-based discovery (no external infrastructure)
  etcd/   — etcd-based discovery (requires etcd)
```

## seed/

| Example | Description | Run |
|---------|-------------|-----|
| `zmq_pub` | Bind a PUB socket and stream data | `cargo run --example zmq_pub --features zmq-beta` |
| `zmq_sub` | Connect to a hardcoded PUB address | `cargo run --example zmq_sub --features zmq-beta` |
| `zmq` | Seed-based discovery — pub registers with seed; sub discovers via seed | `cargo run --example zmq --features zmq-beta` |

Run `zmq_pub` and `zmq_sub` in separate terminals. The `zmq` example runs all three
components (seed, publisher, subscriber) in one process.

## etcd/

| Example | Description | Run |
|---------|-------------|-----|
| `zmq_etcd_pub` | Bind a PUB socket and register address in etcd | `cargo run --example zmq_etcd_pub --features zmq-beta,etcd` |
| `zmq_etcd_sub` | Look up publisher address from etcd and subscribe | `cargo run --example zmq_etcd_sub --features zmq-beta,etcd` |

Start etcd first:

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

Then run `zmq_etcd_pub` and `zmq_etcd_sub` in separate terminals. The publisher's
address lease expires ~30 s after a crash, or immediately on clean shutdown.
