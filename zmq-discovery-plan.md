# ZMQ Service Discovery — Implementation Plan

## Overview

Add seed-based service discovery to the ZMQ adapter (issue #103).

Seeds are lightweight name registries — they handle only discovery traffic, not data. Data flows peer-to-peer over existing PUB/SUB sockets.

```
Publisher → seed: "I'm 'quotes', tcp://hostA:5555"
Subscriber → seed: "who publishes 'quotes'?"
seed → Subscriber: "tcp://hostA:5555"
Subscriber → tcp://hostA:5555  (normal zmq_sub)
```

Multiple seeds provide redundancy: publishers register with all seeds, subscribers try each until one responds.

### Public API

```rust
// Start a seed (background thread, stops on drop)
let seed = start_seed("tcp://0.0.0.0:7777")?;

// Named publisher — binds PUB socket as normal, then registers with seeds
ticker(period)
    .count()
    .zmq_pub_named("quotes", port, &["tcp://seed1:7777", "tcp://seed2:7777"]);

// Discovering subscriber — queries seeds, then connects via normal zmq_sub
let (data, status) = zmq_sub_discover::<Quote>("quotes", &["tcp://seed1:7777"])?;
```

### Design decisions

- **Seed socket type: REP** — discovery is low-volume (once at startup), strictly sequential request-reply, no concurrency needed
- **Wire protocol: bincode** — already a dependency, consistent with existing ZMQ adapter
- **Subscriber resolves at construction time** — `zmq_sub_discover` queries seeds eagerly and returns `Result`; errors surface before the graph is built
- **Publisher registers in `start()`** — must bind the PUB socket first to know the port is available, then register
- **REQ timeout: 3s** — both `register_with_seeds` and `query_seeds` set `rcvtimeo`/`sndtimeo` so dead seeds fail fast
- **Backwards compatible** — `zmq_pub`, `zmq_pub_on`, `zmq_sub` are unchanged

---

## Wire Protocol

```rust
// wingfoil/src/adapters/zmq_seed.rs

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SeedRequest {
    Register { name: String, address: String },
    Lookup   { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SeedResponse {
    Ok,
    Found   { address: String },
    NotFound,
    Error   { message: String },
}
```

---

## Tasks

### Task 1 — Create `zmq_seed.rs`

New file: `wingfoil/src/adapters/zmq_seed.rs`

Contents:
- `SeedRequest` / `SeedResponse` enums (above)
- `Registry` — `HashMap<String, String>` (name → address)
- `SeedHandle` — holds shutdown sender + `JoinHandle`; `Drop` sends shutdown signal and joins
- `start_seed(bind_address: &str) -> anyhow::Result<SeedHandle>` — spawns seed thread
- Seed thread loop: `zmq::poll` on REP socket with 100ms timeout, check shutdown channel on each wakeup; recv request → update registry or lookup → send response
- `pub(crate) fn register_with_seeds(name, address, seeds) -> anyhow::Result<()>` — opens REQ socket per seed (3s timeout), sends `Register`, expects `Ok`; returns `Ok` on first success, `Err` if all fail
- `pub(crate) fn query_seeds(name, seeds) -> anyhow::Result<String>` — opens REQ socket per seed (3s timeout), sends `Lookup`, returns address on `Found`, tries next on `NotFound`/error

Wire `mod.rs`:
```rust
// wingfoil/src/adapters/mod.rs
#[cfg(feature = "zmq-beta")]
pub(crate) mod zmq_seed;
```

### Task 2 — Extend `ZeroMqSenderNode` for named publishing

In `wingfoil/src/adapters/zmq.rs`:

Add:
```rust
struct Registration {
    name: String,
    seeds: Vec<String>,
}
```

Add `registration: Option<Registration>` field to `ZeroMqSenderNode`. Switch from `derive_new` to a hand-written constructor (the struct only has 3 mandatory fields: `src`, `port`, `bind_address`).

Extend `start()`: after `socket.bind(...)`, if `self.registration.is_some()`, call `register_with_seeds(name, &address, seeds)`.

Extend `ZeroMqPub` trait:
```rust
fn zmq_pub_named(&self, name: &str, port: u16, seeds: &[&str]) -> Rc<dyn Node>;
fn zmq_pub_named_on(&self, name: &str, address: &str, port: u16, seeds: &[&str]) -> Rc<dyn Node>;
```

`zmq_pub_named` uses `"127.0.0.1"` as the bind address (same as existing `zmq_pub`). `zmq_pub_named_on` takes an explicit address — document that it must be routable by subscribers.

### Task 3 — Add `zmq_sub_discover`

In `wingfoil/src/adapters/zmq.rs`:

```rust
pub fn zmq_sub_discover<T: Element + Send + DeserializeOwned>(
    name: &str,
    seeds: &[&str],
) -> anyhow::Result<(Rc<dyn Stream<Burst<T>>>, Rc<dyn Stream<ZmqStatus>>)> {
    let address = zmq_seed::query_seeds(name, seeds)?;
    Ok(zmq_sub::<T>(&address))
}
```

### Task 4 — Tests

All tests in `#[cfg(test)]` blocks. Use ports 5570–5595 to avoid collisions with existing tests (5556–5562).

Seed unit tests (in `zmq_seed.rs`):
- `seed_register_and_lookup` — register "foo", lookup "foo", expect Found
- `seed_lookup_unknown` — lookup with no registrations, expect NotFound
- `seed_register_overwrites` — register "foo" twice, second address wins
- `seed_drop_shuts_down` — drop handle, verify thread exits (attempt connect after short sleep, expect failure)

Integration tests (in `zmq.rs`):
- `zmq_named_pub_registers_with_seed` — start seed, run named publisher, directly query seed and assert Found
- `zmq_sub_discover_end_to_end` — full round-trip: seed + named publisher + zmq_sub_discover, assert data received
- `zmq_sub_discover_multiple_seeds_first_down` — first seed absent, second live; assert Ok
- `zmq_sub_discover_no_seed_returns_error` — no seed running, assert Err
- `zmq_sub_discover_name_not_found` — seed running but name not registered, assert Err
- `zmq_named_pub_historical_mode_fails` — assert error mentioning "real-time"

### Task 5 — Example

New file: `wingfoil/examples/messaging/zmq_discover.rs`

Demonstrates: start seed, start named publisher in one thread, discover and subscribe in another, assert data received.

Add to `wingfoil/Cargo.toml`:
```toml
[[example]]
name = "zmq_discover"
path = "examples/messaging/zmq_discover.rs"
required-features = ["zmq-beta"]
```

---

## Files Changed

| File | Change |
|------|--------|
| `wingfoil/src/adapters/zmq_seed.rs` | New — seed thread, wire protocol, helpers |
| `wingfoil/src/adapters/zmq.rs` | Extend `ZeroMqSenderNode` + `ZeroMqPub`; add `zmq_sub_discover` |
| `wingfoil/src/adapters/mod.rs` | Add `pub(crate) mod zmq_seed` under `zmq-beta` |
| `wingfoil/Cargo.toml` | Add `zmq_discover` example |
| `wingfoil/examples/messaging/zmq_discover.rs` | New example |
