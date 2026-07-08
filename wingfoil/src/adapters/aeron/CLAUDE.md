# Aeron Adapter

Subscribes to an Aeron channel and emits `Burst<T>` (`aeron_sub_fragment`),
and publishes serialised values to an Aeron channel (`AeronPub::aeron_pub`).
Aeron is a low-latency UDP/IPC message transport; this adapter wraps it as
wingfoil source and sink nodes.

## Module Structure

```
aeron/
  mod.rs               # AeronMode, AeronSubOptions, aeron_sub_fragment* factories, public re-exports
  transport.rs         # AeronSubscriberBackend / AeronPublisherBackend traits + test mocks
  rusteron_backend.rs  # `aeron` feature: rusteron-client C++ FFI backend (production)
  aeron_rs_backend.rs  # `aeron-rs` feature: pure-Rust aeron-rs backend (experimental)
  sub_fragment_node.rs # Burst<T> subscriber surface (spin + threaded; typed parser with FragmentHeader access)
  pub_node.rs          # AeronPub trait + publisher node (offer + status variants)
  status.rs            # AeronStatus enum (Disconnected / Connected / BackPressured / Closed)
  status_stream.rs     # AeronStatusStream — reactive Burst<AeronStatus> side-channel
  buffer.rs            # FragmentBuffer (borrowed read view) + ClaimBuffer (zero-copy write)
  channel.rs           # ChannelUri builder/validator (IPC, UDP, MDC)
  error.rs             # TransportError (BackPressure / Connection / Backend / Invalid)
  integration_tests.rs # Integration tests (requires a media driver, gated by feature)
  CLAUDE.md            # This file
```

## Backends

Enable **exactly one** backend feature:

| Feature         | Crate             | Toolchain                       | Status                |
|-----------------|-------------------|---------------------------------|-----------------------|
| `aeron`         | `rusteron-client` | cmake ≥3.30, clang, uuid, libbsd| Recommended / production |
| `aeron-rs`      | `aeron-rs`        | pure Rust, none                 | Experimental — see lock warning |

`aeron-driver` additionally embeds a media driver (`rusteron-media-driver`) in
process and implies `aeron`. Without it, point the client at an externally
running media driver (the normal production topology).

## Key Components

### Subscribing — `aeron_sub_fragment`

- `aeron_sub_fragment(subscriber, parser, opts)` — typed parser
  `FnMut(&FragmentBuffer) -> Result<Option<T>, TransportError>` with access to
  the per-fragment `FragmentHeader` (position, session_id, stream_id); emits
  `Burst<T>`.
- `aeron_sub_fragment_with_status(...)` — returns `(data_stream, status_stream)`;
  the status stream emits `Burst<AeronStatus>` on connect/disconnect transitions.
  In threaded mode the poll thread multiplexes data and status over the single
  receiver channel (`AeronItem`); status is sampled at the poll cadence rather
  than per graph cycle.

The `subscriber` handle comes from `AeronHandle::subscription(channel, stream_id, timeout)`
(rusteron) or `AeronRsHandle::subscription(...)` (aeron-rs). Constructing it
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

Fluent trait on `Rc<dyn Stream<Burst<T>>>`:

- `aeron_pub(publisher, serialize)` — offer every burst item each cycle.
- `aeron_pub_with_status(...)` — also returns a status stream. `Closed` is
  terminal and checked first.

The `publisher` handle comes from `AeronHandle::publication(...)` /
`AeronRsHandle::publication(...)`.

## ⚠️ Design note: `aeron-rs` locks on the graph thread

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
- The `aeron-rs` subscriber reports `supports_graph_thread_poll() == false`,
  so `aeron_sub_fragment` **automatically downgrades `AeronMode::Spin` to
  `Threaded`** for it (logging a warning) — the subscriber lock can only ever run
  on the background poll thread. `Spin` is unreachable for this backend by
  construction.
- The publisher has no threaded mode and always locks on the calling thread;
  there is no automatic downgrade for the publish side, so avoid the
  `aeron-rs` publisher on latency-sensitive paths.

This is why the `aeron-rs` backend is considered experimental and is excluded
from the `full` feature set.

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
   cargo clippy -p wingfoil --features aeron-rs   --all-targets -- -D warnings
   ```

## Gotchas

- **A media driver must be running** before `AeronHandle::connect()` — there is
  no offline/construction-only path, so unit tests use the `MockSubscriber` /
  `MockPublisher` backends in `transport.rs` rather than a real handle.
- **rusteron pins:** the backend tracks `rusteron-client` / `rusteron-media-driver`
  `0.1.x`; the FFI surface changes between minor versions.
- **`aeron` needs cmake ≥3.30** (Ubuntu 24.04 ships 3.28) plus clang, uuid-dev,
  libbsd-dev. Missing/old cmake yields a clear build-script error; the pure-Rust
  `aeron-rs` backend needs none of this.
- **Threaded status** is propagated in-band via `AeronItem` (data/status
  multiplexed over the receiver channel); the data node demuxes and replays
  transitions into the shared `AeronStatusStream`, sampled at the poll cadence.
