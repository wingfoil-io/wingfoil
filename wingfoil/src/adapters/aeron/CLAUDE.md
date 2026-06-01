# Aeron Adapter

Subscribes to an Aeron channel and emits `Burst<T>` (`aeron_sub` / `aeron_sub_burst`),
and publishes serialised values to an Aeron channel (`AeronPub::aeron_pub`).
Aeron is a low-latency UDP/IPC message transport; this adapter wraps it as
wingfoil source and sink nodes.

## Module Structure

```
aeron/
  mod.rs               # AeronMode, AeronSubOptions, aeron_sub* factories, public re-exports
  transport.rs         # AeronSubscriberBackend / AeronPublisherBackend traits + test mocks
  rusteron_backend.rs  # `aeron` feature: rusteron-client C++ FFI backend (production)
  aeron_rs_backend.rs  # `aeron-rs-beta` feature: pure-Rust aeron-rs backend (experimental)
  sub_spin.rs          # Spin-mode subscriber node (polls in cycle() on the graph thread)
  sub_threaded.rs      # Threaded-mode subscriber node (background thread + ReceiverStream)
  sub_burst_node.rs    # Burst<T> subscriber surface (typed parser with FragmentHeader access)
  pub_node.rs          # AeronPub trait + publisher node (offer, dedup, status variants)
  status.rs            # AeronStatus enum (Disconnected / Connected / BackPressured / Closed)
  status_stream.rs     # AeronStatusStream — reactive Burst<AeronStatus> side-channel
  buffer.rs            # FragmentBuffer (borrowed read view) + ClaimBuffer (zero-copy write)
  channel.rs           # ChannelUri builder/validator (IPC, UDP, MDC)
  discovery.rs         # Named-endpoint registry + aeron_*_named factories
  external.rs          # ExternalSource escape hatch for caller-owned poll loops
  error.rs             # TransportError (BackPressure / Connection / Backend / Invalid)
  integration_tests.rs # Integration tests (requires a media driver, gated by feature)
  CLAUDE.md            # This file
```

## Backends

Enable **exactly one** backend feature:

| Feature         | Crate             | Toolchain                       | Status                |
|-----------------|-------------------|---------------------------------|-----------------------|
| `aeron`         | `rusteron-client` | cmake ≥3.30, clang, uuid, libbsd| Recommended / production |
| `aeron-rs-beta` | `aeron-rs`        | pure Rust, none                 | Experimental — see lock warning |

`aeron-driver` additionally embeds a media driver (`rusteron-media-driver`) in
process and implies `aeron`. Without it, point the client at an externally
running media driver (the normal production topology).

## Key Components

### Subscribing — `aeron_sub` / `aeron_sub_burst`

- `aeron_sub(subscriber, parser, mode)` — `parser: FnMut(&[u8]) -> Option<T>`,
  emits `Burst<T>`.
- `aeron_sub_burst(subscriber, parser, opts)` — typed parser
  `FnMut(&FragmentBuffer) -> Result<Option<T>, TransportError>` with access to
  the per-fragment `FragmentHeader` (position, session_id, stream_id).
- `aeron_sub_burst_with_status(...)` — returns `(data_stream, status_stream)`;
  the status stream emits `Burst<AeronStatus>` on connect/disconnect transitions
  (spin mode only — threaded mode returns a default-state placeholder).
- `aeron_sub_burst_named(...)` — resolves the subscriber from the named-endpoint
  registry (see `discovery.rs`).

The `subscriber` handle comes from `AeronHandle::subscription(channel, stream_id, timeout)`
(rusteron) or `AeronRsHandle::subscription(...)` (aeron-rs-beta). Constructing it
**requires a live media driver** — there is no offline construction path.

### Polling modes — `AeronMode`

- `Spin` *(primary)* — polls Aeron inside `cycle()` on the graph thread via
  `state.always_callback()`. Zero thread-crossing latency; burns one CPU core.
  Returns `Ok(true)` only when fragments arrived so downstream ticks reactively.
- `Threaded` *(secondary)* — polls on a dedicated background thread and delivers
  via `ReceiverStream` channel. One channel-hop of latency; frees the graph
  thread. `RunMode::RealTime` only.

`AeronSubOptions { mode, fragment_limit }` bundles the choice; `fragment_limit`
caps fragments processed per `poll()` (default `DEFAULT_FRAGMENT_LIMIT = 256`).

### Publishing — `AeronPub`

Fluent trait on `Rc<dyn Stream<Burst<T>>>` (and the single-value `Rc<dyn Stream<T>>`):

- `aeron_pub(publisher, serialize)` — offer every value each cycle.
- `aeron_pub_dedup(...)` — skip offering when the value equals the last published
  (latest-wins on back-pressure: `last_published` is not advanced on a
  back-pressured offer).
- `aeron_pub_with_status(...)` / `aeron_pub_dedup_with_status(...)` — also return
  a status stream. `Closed` is terminal and checked first.

The `publisher` handle comes from `AeronHandle::publication(...)` /
`AeronRsHandle::publication(...)`.

### ExternalSource escape hatch

`ExternalSource<T>` lets a caller-owned Aeron poll loop push decoded values into
the graph (zero-copy SBE decode inside the poll callback, custom idle strategy,
single-threaded HFT control flow). See `examples/aeron_external_source.rs`.

## ⚠️ Design note: `aeron-rs-beta` locks on the graph thread

The `aeron-rs` crate returns `Subscription` / `Publication` as `Arc<Mutex<…>>`
and shares them with its own background client-conductor thread, which locks
them on every publisher connect/disconnect (`on_available_image` /
`on_unavailable_image`). The lock is the live synchronisation point between our
poll and that conductor, so it **cannot be hoisted to wiring time** — it must be
taken and released on every `poll()` / `offer()`. In `AeronMode::Spin` those
calls run in the graph `cycle()`, so the contended lock lands on the graph
thread, violating wingfoil's "no locks in `cycle()`" invariant.

Guidance:
- Use the `aeron` (rusteron) backend for low-latency / production — its
  `poll()` / `offer()` are genuinely lock-free.
- If you must use `aeron-rs-beta`, prefer `AeronMode::Threaded` so the
  subscriber lock runs on the background poll thread. The publisher has no
  threaded mode and always locks on the calling thread.

This is why `aeron-rs-beta` is named `-beta` and is excluded from the `full`
feature set.

## Pre-Commit Requirements

1. **Run integration tests (requires Docker for the media driver):**

   ```bash
   cargo test --features aeron-integration-test -p wingfoil \
     -- --test-threads=1 aeron::integration_tests
   ```

   The tests start a `neomantra/aeron-cpp-debian` container running `aeronmd`,
   bind-mounting `/dev/shm` so the host client shares the CNC file over
   `aeron:ipc`.

2. **Format and lint both backends:**

   ```bash
   cargo fmt --all
   cargo clippy -p wingfoil --features aeron,aeron-driver --all-targets -- -D warnings
   cargo clippy -p wingfoil --features aeron-rs-beta   --all-targets -- -D warnings
   ```

## Gotchas

- **A media driver must be running** before `AeronHandle::connect()` — there is
  no offline/construction-only path, so unit tests use the `MockSubscriber` /
  `MockPublisher` backends in `transport.rs` rather than a real handle.
- **rusteron pins:** the backend tracks `rusteron-client` / `rusteron-media-driver`
  `0.1.x`; the FFI surface changes between minor versions.
- **`aeron` needs cmake ≥3.30** (Ubuntu 24.04 ships 3.28) plus clang, uuid-dev,
  libbsd-dev. Missing/old cmake yields a clear build-script error; the pure-Rust
  `aeron-rs-beta` backend needs none of this.
- **Threaded status** is a default-state placeholder — only spin mode wires a
  live `AeronStatusStream`.
