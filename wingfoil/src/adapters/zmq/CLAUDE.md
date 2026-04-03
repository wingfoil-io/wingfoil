# ZMQ Adapter

Real-time pub/sub messaging using ØMQ sockets (`zmq_sub` / `zmq_pub`) with
optional registry-based service discovery (`SeedRegistry`, `EtcdRegistry`).

## Module Structure

```
zmq/
  mod.rs               # ZmqStatus, ZmqEvent, public re-exports, module doc
  read.rs              # zmq_sub() — subscriber producer
  write.rs             # ZeroMqSenderNode, ZeroMqPub trait — publisher consumer
  registry.rs          # ZmqRegistry/ZmqHandle traits, ZmqPubRegistration/ZmqSubConfig,
                       #   SeedRegistry, EtcdRegistry (cfg-gated)
  seed.rs              # Seed protocol: SeedHandle, start_seed(), register/query helpers
  integration_tests.rs # All tests (gated by feature flags)
  CLAUDE.md            # This file
```

## Key Design Decisions

### Threading model: ReceiverStream (not produce_async)

ZMQ sockets are synchronous and poll-based. The subscriber uses `ReceiverStream`
which dedicates a real OS thread per subscriber rather than `produce_async` (which
would require tokio and wrapping every `zmq::poll` call in `spawn_blocking`).

The `zmq-beta` feature deliberately does not depend on `async`; adding a tokio
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

### Seed protocol

Seeds use REQ/REP (strictly one-at-a-time) because discovery is low-volume —
it happens once at startup, not in the data path. Bincode is used for wire
encoding (already a dependency). Seeds use `zmq::poll` with a 10 ms timeout
so the shutdown flag is checked regularly without spinning.

`register_with_seeds` and `query_seeds` set a 3-second `rcvtimeo`/`sndtimeo`
so dead seeds fail fast rather than blocking indefinitely.

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
method that works for all three use cases without overloading:

```rust
// ZmqPubRegistration
stream.zmq_pub(5556, ())                           // no registration
stream.zmq_pub(5556, ("quotes", seeds_registry))   // named + backend

// ZmqSubConfig
zmq_sub::<T>("tcp://host:5556")?                   // direct address
zmq_sub::<T>(("quotes", seeds_registry))?          // discovery
```

### `SeedRegistry`

Always compiled. Wraps the existing seed protocol; `revoke()` is a no-op
(seeds don't have persistent state that needs cleanup).

### `EtcdRegistry` (requires `etcd` feature)

Stores the address in etcd under a 30 s lease. A dedicated `std::thread` runs a
keepalive loop every 10 s using its own `new_current_thread` tokio runtime.
`ZmqHandle::revoke()`:
1. Sets the shutdown `AtomicBool` to stop the keepalive thread.
2. Joins the keepalive thread (may block up to 10 s).
3. Calls `lease_revoke` so the key disappears immediately.

### When to prefer etcd over seed

| Factor | Seed | etcd |
|--------|------|------|
| Infra | None (built-in) | Requires etcd |
| Fault tolerance | Single-seed SPOF | HA cluster |
| Lease / TTL | No (manual cleanup) | Yes (auto-expiry) |
| Debuggability | ZMQ REQ/REP | `etcdctl get <name>` |
| Use case | Simple setups | Existing etcd infra, HA |

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features

# ZMQ tests (no Docker needed)
cargo test --features zmq-beta-integration-test -p wingfoil \
  -- --test-threads=1 zmq::integration_tests

# etcd discovery tests (requires Docker)
cargo test --features zmq-etcd-integration-test -p wingfoil \
  -- --test-threads=1 zmq::integration_tests::etcd_tests
```

## Integration Test Port Allocation

| Range      | Tests                                         |
|------------|-----------------------------------------------|
| 5556–5562  | Core pub/sub tests                            |
| 5570–5573  | Seed unit tests                               |
| 5580–5595  | Seed discovery integration tests              |
| 5596–5610  | etcd discovery integration tests              |
| 9001–9100  | Addresses registered with seeds (test data)   |

Do not use these ports for other tests in the workspace.
