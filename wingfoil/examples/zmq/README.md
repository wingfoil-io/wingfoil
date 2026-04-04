# ZMQ Examples

All ZMQ adapter examples live here.

| Example | Description | Features |
|---------|-------------|----------|
| `zmq_pub` | Simple publisher — binds a PUB socket and streams data | `zmq-beta` |
| `zmq_sub` | Simple subscriber — connects to a hardcoded PUB address | `zmq-beta` |
| `zmq` | Seed-based discovery — publisher registers with a seed; subscriber discovers via seed | `zmq-beta` |
| `zmq_etcd_discovery` | etcd-based discovery — publisher registers address in etcd; subscriber looks it up | `zmq-beta,etcd` |

## Run

```sh
# Direct pub/sub (run in separate terminals)
RUST_LOG=info cargo run --example zmq_pub --features zmq-beta
RUST_LOG=info cargo run --example zmq_sub --features zmq-beta

# Seed-based discovery (all-in-one process)
RUST_LOG=info cargo run --example zmq --features zmq-beta

# etcd-based discovery (requires etcd on localhost:2379)
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0

RUST_LOG=info cargo run --example zmq_etcd_discovery --features zmq-beta,etcd
```

## Discovery modes

**Seed-based** (`zmq`, `zmq_pub`/`zmq_sub` with a seed): a lightweight in-process
registry — no external infrastructure required. The seed stops when dropped.

**etcd-based** (`zmq_etcd_discovery`): publisher writes its address to etcd under a
lease; the address disappears ~30 s after a crash, or immediately on clean shutdown.
