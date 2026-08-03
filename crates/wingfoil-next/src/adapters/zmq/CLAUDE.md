# ZMQ Adapter (wingfoil-next)

Real-time pub/sub over ØMQ sockets, with optional service discovery. Ports
legacy `wingfoil::adapters::zmq` onto the Op model.

`zmq_sub` is the **reference implementation of `source_at_start`** — the
sync-streaming-client shape that `/new-adapter-next` step 7 tells you to copy.

## Layout

```
adapters/
  zmq.rs               # ZmqStatus, the ZmqEvent envelope, zmq_sub, ZmqPubState, ZeroMqPub
  zmq/
    registry.rs        # ZmqRegistry / ZmqHandle traits, config wrappers, EtcdRegistry
    CLAUDE.md          # this file
```

## Feature gating

```toml
zmq = ["dep:zmq", "dep:bincode", "dep:serde"]
zmq-integration-test = ["zmq"]                              # real sockets, no service
zmq-etcd-integration-test = ["zmq", "etcd", "dep:testcontainers"]
```

**No `async`** — deliberately, matching legacy: ZeroMQ sockets are synchronous
and poll-based, so the subscriber uses a background thread over the `channel`
layer. The `zmq` crate links the system `libzmq`.

Discovery is a **pluggable backend** behind the `ZmqRegistry` trait: `zmq`
alone works for direct addresses; with the `etcd` feature also on,
`EtcdRegistry` compiles in as a backend.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `zmq_sub(g, run_mode, config)` | source | `Result<(Stream<Burst<T>>, Stream<ZmqStatus>)>` |
| `ZeroMqPub::zmq_pub(port, registration)` | sink trait | binds on `127.0.0.1`; returns `Stream<()>` |
| `ZeroMqPub::zmq_pub_on(address, port, registration)` | sink trait | routable bind |

Both endpoints take `impl Into<…>` config wrappers (`ZmqSubConfig` /
`ZmqPubRegistration`) with `From` impls, so one signature serves a bare address
*or* a `(name, registry)` discovery pair:

```rust
stream.zmq_pub(5556, ());                                   // no registration
stream.zmq_pub(5556, ("quotes", EtcdRegistry::new(conn)));  // register in etcd

zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, "tcp://host:5556")?;
zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, ("quotes", EtcdRegistry::new(conn)))?;
```

## What to know before changing it

- **`zmq_sub` wiring is pure**: reject `HistoricalFrom`, resolve/validate the
  config (parse or registry lookup). The socket connect and thread spawn happen
  in the `source_at_start` setup closure at graph `start()`, and the returned
  `StopHandle` (a `ThreadStopGuard` flipping an `AtomicBool`) stops the thread
  at teardown. Keep that split — it is what makes wiring unit-testable without
  a live socket and surfaces connect errors at run start with node context.
- **Realtime only.** A historical run would block-collect the never-closing
  stream and deadlock at `start` (register B2, ratified).
- **Data and status are multiplexed in-band** over one channel (the internal,
  **private** `ZmqEvent<T>` envelope) and split back into the two streams, so a
  `Connected` transition stays ordered before the messages that followed it. A
  ZMQ *monitor* socket alongside the data socket is where transitions come
  from. `ZmqStatus` emits **only on transition**.
- **`zmq_pub` binds at graph `start()`, not lazily on the first publish**
  (register **A3**). This matters: a fresh `SUB` peer misses messages sent
  before its subscription filter propagates (ZeroMQ's *slow-joiner* problem),
  so the publisher watches its own monitor socket for the first accepted
  connection and **buffers** outgoing messages until the subscriber is ready
  (up to `BUFFER_TIMEOUT`, plus a short propagation delay). Binding lazily
  compressed the whole handshake into the first publish and made
  `first_message_not_dropped` flaky under CI load (register **A6** — the same
  bug, diagnosed).
- **The publisher errors under historical replay**, it does not no-op. That is
  legacy parity and a deliberate exception to the skill's exporter default:
  publishing fast-forwarded historical data to a live socket is meaningless.
  The abort happens at `start()`, naming the run mode, **before** touching the
  registry.
- **The wire envelope is `bincode` and next-local** — a next publisher
  interoperates with a next subscriber but is **not** wire-compatible with a
  legacy/Python `wingfoil` peer (register **C2**, deferred with the Python
  bindings).
- No locks on the graph path: the subscriber thread talks to the graph only
  through `ChannelSender`.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `zmq.rs` — three
items: `zmq_sub` takes a `GraphBuilder` + `RunMode` (needed for the wiring
rejection, since next's channel is bimodal); `zmq_pub` returns `Stream<()>`
with bind/registration/run-mode-check at `start()`; and the next-local wire
envelope (C2). Two smaller reductions: `ZmqEvent<T>` is private here (legacy
exposed it, but it is purely an internal transport detail), and `ZmqStatus`
additionally derives `Eq`. Every legacy capability — sub with a status stream,
pub with slow-joiner buffering, the `ZmqRegistry`/`EtcdRegistry` backend,
`zmq_pub_on` — is preserved.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/zmq_adapter.rs` | `#![cfg(feature = "zmq")]` | nothing |
| `tests/zmq_integration.rs` | `#![cfg(feature = "zmq-integration-test")]` | real loopback sockets, no service |
| `tests/zmq_etcd_integration.rs` | `#![cfg(feature = "zmq-etcd-integration-test")]` | an etcd container |
| `tests/zmq_cross_engine_integration.rs` | `#![cfg(feature = "zmq-cross-engine-test")]` | real sockets + the legacy crate's zmq adapter |
| `tests/zmq_cross_lang_integration.rs` | `#![cfg(feature = "zmq-cross-lang-test")]` | real sockets + `maturin develop`; the `etcd` half also needs a container |

Port allocation, so tests never collide when run in parallel:

| Range | Tests |
|---|---|
| 5701–5702 | `zmq_adapter.rs` |
| 5711–5716 | `zmq_integration.rs` |
| 5721–5724 | `zmq_etcd_integration.rs` |
| 5731–5732 | `zmq_cross_engine_integration.rs` |
| 5741–5744 | `zmq_cross_lang_integration.rs` |
| 5599–5602 | `wingfoil-next-python`'s `test_zmq.py` |

```bash
cargo test -p wingfoil-next --features zmq --test zmq_adapter
cargo test -p wingfoil-next --features zmq-integration-test -- --test-threads=1
cargo test -p wingfoil-next --features zmq-etcd-integration-test -- --test-threads=1
cargo test -p wingfoil-next --features zmq-cross-engine-test -- --test-threads=1
# needs `maturin develop` in crates/wingfoil-next-python first
cargo test -p wingfoil-next --features zmq-cross-lang-test -- --test-threads=1
cargo test -p wingfoil-next --features zmq-cross-lang-etcd-test -- --test-threads=1
```

`zmq_integration.rs` ports the core pub/sub tests from legacy's
`integration_tests.rs`; `zmq_etcd_integration.rs` ports its `etcd_tests`
module. `first_message_not_dropped` is the slow-joiner regression guard — if
you touch bind timing, run it repeatedly under load.

### The wire contract, and the two files that hold it

`WireMessage<T>` is **byte-compatible with legacy's `channel::Message<T>`**, so
a next publisher is read by a legacy or legacy-Python subscriber and vice
versa. `bincode` encodes an enum as an index into declaration order, so
**reordering those variants silently reinterprets every message** — no error,
just wrong values. Three tiers guard it:

| Where | What it proves |
|---|---|
| `wire_format_matches_legacy_message` (unit) | next's own encoding, against golden bytes |
| `zmq_cross_engine_integration.rs` | legacy actually agrees — real sockets, both directions. **Retires with the legacy tree** |
| `zmq_cross_lang_integration.rs` | next-Rust ↔ next-Python agree. Survives the cutover |

The golden-bytes test is deliberately longhand rather than a cross-check
against legacy's encoder: legacy's `Message` is `pub(crate)` and unreachable
from here, and a golden encoding catches drift on *either* side where a
cross-check would follow both if they moved together.

The cross-language tests **fail** rather than skip when `import wingfoil_next`
does not work — a silently-skipped interop test is how a broken binding reaches
a release green.

**Workflow:** `.github/workflows/zmq-next-integration.yml` (in
`integration-tests.yml`) runs the integration, cross-engine and cross-language
feature sets. The cross-language leg builds the Python bindings with `maturin
develop` first.

## Example

`examples/zmq_adapter.rs`, `required-features = ["zmq"]`.

## Python

`wingfoil-next-python` feature `zmq = ["wingfoil-next/zmq", "_common"]`.
**In `all-adapters` and in the wheel** — `zmq-sys` builds libzmq from source
rather than linking a system one, so a C toolchain is all it needs.

- Entry points, `#[pyadapter]` in `src/adapters/zmq.rs`: `zmq_sub`, `zmq_pub`,
  and — only when the binding's `etcd` feature is also on — `zmq_sub_etcd`,
  `zmq_pub_etcd`.
- `zmq_sub` is the binding that made `#[pyadapter]` accept a **tuple return**:
  `data, status = wingfoil_next.zmq_sub(g, "tcp://localhost:5556")`. `status`
  ticks only on transition, carrying the string `"connected"` /
  `"disconnected"` (a string, not a `#[pyclass]` enum — the postgres
  convention).
- Payloads cross as **`bytes`**: the Rust source is generic over a
  `DeserializeOwned` record and the binding instantiates it at `Vec<u8>`,
  exactly as the legacy binding did.
- Tests: `tests/test_zmq.py`, **no marker** — runs by default in
  `next-python-test.yml`.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features zmq
cargo test -p wingfoil-next --features zmq-integration-test -- --test-threads=1
```
