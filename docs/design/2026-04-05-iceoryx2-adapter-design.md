---
date: 2026-04-05
last_reviewed: 2026-04-06
topic: iceoryx2-adapter-design
status: in_review
---

# iceoryx2 Adapter — Design

This document describes the design of Wingfoil’s `iceoryx2` adapter implementation:

- `wingfoil/src/adapters/iceoryx2/mod.rs`
- `wingfoil/src/adapters/iceoryx2/read.rs`
- `wingfoil/src/adapters/iceoryx2/write.rs`

For requirements and acceptance criteria, see `docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`.

## Design Summary

### Public API Surface (Behind `iceoryx2-beta`)

- Subscriber:
  - `iceoryx2_sub<T>(service_name)` (defaults to `Ipc + Spin`)
  - `iceoryx2_sub_with<T>(service_name, variant)`
  - `iceoryx2_sub_opts<T>(service_name, Iceoryx2SubOpts)`
  - `iceoryx2_sub_slice*` variants for bytes (`Burst<Vec<u8>>`)
- Publisher:
  - `iceoryx2_pub<T>(upstream, service_name)` (defaults to `Ipc`)
  - `iceoryx2_pub_with<T>(upstream, service_name, variant)`
  - `iceoryx2_pub_slice*` for bytes (`Burst<Vec<u8>>`)
  - `iceoryx2_pub_slice_with(upstream, service_name, variant)`

### Key Types

- `Iceoryx2ServiceVariant`: `Ipc` vs `Local`
- `Iceoryx2Mode`: `Spin` / `Threaded` / `Signaled`
- `Iceoryx2SubOpts`: includes `history_size` and mode/variant
- `Iceoryx2PubOpts`: publisher options (variant + `history_size`)
- `Iceoryx2PubSliceOpts`: slice publisher options (`history_size` + `initial_max_slice_len`)
- `Iceoryx2ServiceContract`: shared service-level pub/sub contract (history + derived buffers)
- `Iceoryx2SliceContract`: shared slice contract (pub/sub contract + max slice len)
- `FixedBytes<N>`: fixed-capacity byte payload for `ZeroCopySend` use cases

## Transport Variants

### `Ipc` Variant

Uses `ipc::Service` and shared memory backing (commonly `/dev/shm` or `/tmp/iceoryx2`).
This is the intended production path for cross-process graphs.

### `Local` Variant

Uses `local::Service` and heap-based backing.
This is used to make tests deterministic and daemonless by default.

## Subscriber Execution Modes

### `Spin` Mode

- Subscriber port is created during `start()`.
- The Wingfoil node receives samples directly inside `cycle()`.
- `update_connections()` is called periodically during `cycle()` to handle late joiners.
- The node enables `always_callback()` so it is scheduled even without upstream activity.

Intended use:
- lowest latency, tolerant of high CPU usage.

### `Threaded` Mode

- A background thread owns the subscriber port.
- Samples are read in a loop and forwarded to the graph thread via `ChannelSender`.
- When idle, the thread sleeps briefly (`~10µs`) to reduce CPU usage.
- `cycle()` drains the channel with `try_recv()`.
- The node enables `always_callback()` so it is scheduled to drain the channel.

Intended use:
- reduces impact on the graph thread, accepts added latency of a channel hop.

### `Signaled` Mode

- A background thread uses a `WaitSet` attached to an iceoryx2 Event listener.
- Publisher sends notifications to `"{service_name}.signal"`.
- On wake, the thread drains `subscriber.receive()` and forwards values over the channel.
- `cycle()` drains the channel with `try_recv()`.
- The node enables `always_callback()` so it is scheduled to drain the channel.

Intended use:
- event-driven / low CPU idle behavior, acceptable µs-level latency.

## Thread Lifecycle

`Threaded` and `Signaled` modes spawn background threads. For production correctness and test stability:
- threads must be stopped on graph shutdown
- threads must not outlive the graph (join handles should be owned and joined deterministically)

Implementation note:
- Wingfoil stores join handles for background subscriber threads and joins them during `stop()` so threads cannot outlive the graph teardown.

Review tracking:
- PR #176 review closure work is tracked in `docs/plans/2026-04-05-003-iceoryx2-pr176-comment-closure-plan.md`.
- Structured PR audit (skills + agents) is tracked in `docs/plans/2026-04-06-005-pr176-skill-audit-plan.md`.

## Connection Management

iceoryx2 is decentralized and does not run background threads for connection management.
To avoid short-lived publisher/subscriber “misses”, both sides call `update_connections()`:

- On port creation (`start()`), to establish initial connectivity.
- Periodically while running, to handle late subscribers and short-lived publishers.

Current strategy:
- Subscriber (`Spin`): call every ~10 cycles.
- Subscriber thread loops: call every ~100 loop iterations, and in Signaled mode also after each WaitSet wake.
- Publisher: call in `start()` and periodically during `cycle()` (typed + slice publishers).

Note: publisher-side connection refresh is important for short-lived graphs/tests, otherwise a subscriber may not connect before the graph stops.

## History / Buffering

`Iceoryx2SubOpts.history_size` is used to set:

- `publish_subscribe::<T>().history_size(history_size)`
- `subscriber_max_buffer_size(history_size.max(16))`
- `subscriber_builder().buffer_size(history_size.max(1))`

This enables late joiners to receive recent samples (bounded by history configuration).

Important: `history_size` is a publish/subscribe **service configuration** in iceoryx2. If one side creates the service with one `history_size` and the other side tries to `open_or_create()` with a different value, the open can fail. For IPC deployments, treat `history_size` as part of the service contract (configure consistently across publisher/subscriber and across languages).

Implementation note:
- Wingfoil exposes publisher options (`Iceoryx2PubOpts`, `Iceoryx2PubSliceOpts`) to make this service contract explicit and configurable.
  - `Iceoryx2SubOpts::contract()` / `Iceoryx2PubOpts::contract()` / `Iceoryx2PubSliceOpts::contract()` provide a deterministic view of the exact service-level settings derived from options.

Python note:
- `Stream.iceoryx2_pub(...)` also exposes `history_size` and `initial_max_slice_len` so Python publishers can match the same service contract as Rust subscribers (and vice versa).
- Python publish values must be `bytes` or `list[bytes]`. Invalid types surface as a Python `TypeError` (rather than silently logging and emitting an empty burst).
  - Implementation detail: Wingfoil wraps these input-validation errors as a typed Rust error and converts them to `PyTypeError` at the PyO3 boundary (so callers can catch reliably).

Defaults (for interop predictability):
- Default `history_size`: `5`
- Default `subscriber_max_buffer_size`: `max(history_size, 16)`
- Default `initial_max_slice_len`: `128 * 1024` bytes

Operational note:
- Service open/create failures include service-config context (service name, variant, `history_size`, and `subscriber_max_buffer_size`) to make mismatch issues diagnosable in production logs.
  - In Rust, this context is available as a structured `Iceoryx2Error::ServiceOpenFailed { .. }` so callers can reliably extract it without parsing strings.
  - When we can detect a configuration mismatch, Wingfoil surfaces `Iceoryx2Error::ServiceConfigMismatch { .. }`.

## Signaling (Event Service)

`Signaled` mode relies on a separate Event service:

- Signal service name: `format!("{service_name}.signal")`
- Publisher creates a `Notifier` for this signal service.
- Subscriber creates a `Listener` for the same signal service and attaches it to a `WaitSet`.

If no notifications are sent, Signaled mode can still make progress via timeouts, but delivery latency depends on wake behavior.

### Event Service Races

In Signaled mode, both publisher and subscriber open/create the Event service `"{service_name}.signal"`.
In practice, these can race during short-lived graphs and may briefly surface as errors like `ServiceInCorruptedState`.

Implementation detail:
- Both sides retry `open_or_create()` a few times with a short sleep to make Signaled mode robust in tests and CI.

## Error Handling

The adapter maps common errors into `Iceoryx2Error`:

- node/service/port creation errors are reported with context
- other errors are wrapped into `Iceoryx2Error::Other(anyhow::Error)`

Guideline:
- tests should assert on error presence and category, not on exact string messages.

### “Config Mismatch” Classification

When opening/creating a service fails, Wingfoil attempts a best-effort classification into:

- `Iceoryx2Error::ServiceConfigMismatch { .. }` when the underlying error strongly suggests incompatibility
- `Iceoryx2Error::ServiceOpenFailed { .. }` for all other open/create failures

Important notes:
- This classification is intentionally conservative; it must not depend on stable upstream error strings.
- The stable invariant is the *structured service contract context* included with the error:
  `service_name`, `variant`, `history_size`, and the derived `subscriber_max_buffer_size`.

## Testing Strategy (TDD)

Default tests should use `Local` to avoid shared-memory dependencies.
IPC behavior should be validated via:

- feature-gated integration tests (`iceoryx2-integration-test`)
- or `#[ignore]` tests run explicitly in an environment with `/dev/shm` and permissions

Test matrix to keep in mind:
- Variant: `Local`, `Ipc`
- Mode: `Spin`, `Threaded`, `Signaled`
- Payload: fixed type (`TestData`), slice (`Vec<u8>`)
- Scenario: steady-state, late-joiner/history, short-lived publisher, graph stop/start

Test authoring note:
- `StreamOperators::collapse()` keeps only the last item of a burst. Use it only when you want
  “latest value” semantics per tick; do not use it in history tests where you need to assert on
  all samples delivered.

Developer workflow note:
- Repo pre-push hooks may run a full `cargo test`. If your iceoryx2 work is unrelated to other
  adapters, prefer fixing upstream test flakiness rather than routinely using `git push --no-verify`.
