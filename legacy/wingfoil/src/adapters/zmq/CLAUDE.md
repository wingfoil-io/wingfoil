# ZMQ Adapter

Real-time pub/sub messaging using ØMQ sockets (`zmq_sub` / `zmq_pub`) with
optional registry-based service discovery (`EtcdRegistry`).

## Module Structure

```
zmq/
  mod.rs               # ZmqStatus, ZmqEvent, public re-exports, module doc
  read.rs              # zmq_sub() — subscriber producer
  write.rs             # ZeroMqSenderNode, ZeroMqPub trait (zmq_pub / zmq_pub_on) — publisher consumer
  registry.rs          # ZmqRegistry/ZmqHandle traits, ZmqPubRegistration/ZmqSubConfig,
                       #   EtcdRegistry (cfg-gated)
  integration_tests.rs # All tests (gated by feature flags)
  CLAUDE.md            # This file
```

## Key Design Decisions

### Threading model: ReceiverStream (not produce_async)

ZMQ sockets are synchronous and poll-based. The subscriber uses `ReceiverStream`
which dedicates a real OS thread per subscriber rather than `produce_async` (which
would require tokio and wrapping every `zmq::poll` call in `spawn_blocking`).

The `zmq` feature deliberately does not depend on `async`; adding a tokio
dependency just to adapt blocking sockets would be unnecessary overhead.

### Pub/sub via MutableNode (not consume_async)

`ZeroMqSenderNode` implements `MutableNode` directly (`start` / `cycle` / `stop`)
rather than `consume_async`. ZMQ binding and sending are synchronous and don't
benefit from an async wrapper.

### Connection monitoring

The subscriber opens a ZMQ monitor socket alongside the data socket. This lets
the graph emit `ZmqStatus::Connected` / `ZmqStatus::Disconnected` events without
polling or external state. Monitor events are delivered on an `inproc://` socket
polled in the same thread as the data socket.

The publisher also opens a monitor socket (watching `ACCEPTED` events) and
buffers outgoing messages until the first subscriber connects — up to 500 ms
(`BUFFER_TIMEOUT`), plus a 50 ms subscription-propagation delay after the TCP
accept — so messages published before the subscriber is ready are not lost.

## Registry-Based Discovery

### `ZmqRegistry` / `ZmqHandle` traits

```rust
pub trait ZmqRegistry: Send + 'static {
    fn register(&self, name: &str, address: &str) -> anyhow::Result<Box<dyn ZmqHandle>>;
    fn lookup(&self, name: &str) -> anyhow::Result<String>;
}
pub trait ZmqHandle: Send {
    fn revoke(&mut self);  // called from stop(); best-effort, errors logged
}
```

### `ZmqPubRegistration` / `ZmqSubConfig` config types

These wrapper types use `From` impls to enable a single `zmq_pub` / `zmq_sub`
method that works for all the use cases without overloading:

```rust
// ZmqPubRegistration
stream.zmq_pub(5556, ())                           // no registration
stream.zmq_pub(5556, ("quotes", etcd_registry))    // named + backend

// ZmqSubConfig
zmq_sub::<T>("tcp://host:5556")?                   // direct address
zmq_sub::<T>(("quotes", etcd_registry))?           // discovery
```

`zmq_pub` binds on `127.0.0.1`; use `zmq_pub_on(address, port, registration)`
to bind a routable address for multi-host deployments (otherwise the address
stored in the registry is unreachable from other machines).

### `EtcdRegistry` (requires `etcd` feature)

Stores the address in etcd under a 30 s lease. A dedicated `std::thread` runs a
keepalive loop every 10 s using its own `new_current_thread` tokio runtime.
`ZmqHandle::revoke()`:
1. Signals a shutdown `tokio::sync::Notify`, which wakes the keepalive loop
   immediately (it `select!`s on the notify vs. the 10 s sleep).
2. Joins the keepalive thread.
3. Calls `lease_revoke` so the key disappears immediately.

### When to use etcd discovery vs direct address

| Factor | Direct address | etcd |
|--------|---------------|------|
| Infra | None | Requires etcd |
| Fault tolerance | Manual | HA cluster |
| Lease / TTL | N/A | Yes (auto-expiry) |
| Debuggability | N/A | `etcdctl get <name>` |
| Use case | Static topology | Dynamic / HA setups |

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features

# ZMQ tests (no Docker needed)
cargo test --features zmq-integration-test -p wingfoil \
  -- --test-threads=1 zmq::integration_tests

# etcd discovery tests (requires Docker)
cargo test --features zmq-etcd-integration-test -p wingfoil \
  -- --test-threads=1 zmq::integration_tests::etcd_tests

# Cross-language tests (requires `maturin develop` in wingfoil-python/)
cargo test --features zmq-cross-lang-test -p wingfoil \
  -- --test-threads=1 zmq::integration_tests::cross_lang_tests

# Cross-language etcd tests (requires Docker + maturin develop)
cargo test --features zmq-cross-lang-etcd-test -p wingfoil \
  -- --test-threads=1 zmq::integration_tests::cross_lang_tests::etcd
```

## Integration Test Port Allocation

| Range      | Tests                              |
|------------|------------------------------------|
| 5556–5563  | Core pub/sub tests                 |
| 5580–5590  | Cross-language integration tests   |
| 5596–5610  | etcd discovery integration tests   |

Do not use these ports for other tests in the workspace.
