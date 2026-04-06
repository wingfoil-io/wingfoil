---
date: 2026-04-05
last_reviewed: 2026-04-06
topic: iceoryx2-adapter-requirements
status: in_review
---

# iceoryx2 Adapter — Requirements

## Context

Wingfoil’s `iceoryx2` adapter provides zero-copy pub/sub transport for Wingfoil graphs.
It is intended for latency-critical pipelines and should support both:

- **Inter-process** transport (shared-memory IPC) for production deployments.
- **Intra-process** transport (heap/local) for deterministic CI, smoke tests, and embedding.

This document captures the requirements and acceptance criteria for the adapter and its tests.

## Goals

- Provide a **simple** public API (`iceoryx2_sub*`, `iceoryx2_pub*`) behind the `iceoryx2-beta` feature.
- Make **IPC the default** for production semantics, while still supporting a **daemonless Local mode**.
- Ensure correctness under common lifecycle patterns (late joiners, short-lived publishers, graph stop/start).
- Offer predictable performance trade-offs across subscriber modes (Spin / Threaded / Signaled).
- Support Python bindings for byte-oriented pub/sub use cases.

## Non-Goals

- Guarantee deterministic delivery ordering across processes beyond what iceoryx2 provides.
- Provide durable persistence; history is an in-memory ring buffer only.
- Support arbitrarily sized payloads without configuration (slice size limits exist).
- Provide a cross-platform story beyond Linux-focused shared memory constraints.

## Functional Requirements

### Transport Variants

- **FR-1**: Support `Ipc` variant for shared-memory pub/sub across processes.
- **FR-2**: Support `Local` variant for intra-process pub/sub with no reliance on `/dev/shm`.
- **FR-3**: Keep `Ipc` as the default behavior for `iceoryx2_sub()` / `iceoryx2_pub()`.

### Subscriber Modes

- **FR-4**: Support `Spin` mode where receiving happens in the Wingfoil graph thread `cycle()`.
- **FR-5**: Support `Threaded` mode where receiving happens on a background thread and delivers via a channel.
- **FR-6**: Support `Signaled` mode where the receiver blocks on a WaitSet and wakes on publisher notifications.

### Signaling Robustness

- **FR-6a**: `Signaled` mode must tolerate publisher/subscriber races when opening/creating the `"{service_name}.signal"` Event service (e.g., retry on transient service-state errors).

### Connection Management

- **FR-7**: Call `update_connections()` on startup for both publisher and subscriber ports.
- **FR-8**: Periodically call `update_connections()` while running to handle late subscribers and short-lived publishers.

### History / Buffering

- **FR-9**: Provide a configurable `history_size` for subscriber creation so late joiners can receive recent samples.
- **FR-10**: Ensure subscriber buffer sizes are derived from `history_size` (with sane minimums).

Notes:
- `history_size` is a **service-level** publish/subscribe configuration in iceoryx2. All participants opening/creating the same service must agree on compatible settings, otherwise `open_or_create()` may fail.
- A shared contract helper should exist to derive consistent service settings (`history_size` and derived buffers) across publishers/subscribers and languages.
  - In Wingfoil, this contract is represented as `Iceoryx2ServiceContract { history_size, subscriber_max_buffer_size }` where `subscriber_max_buffer_size` is derived as `max(history_size, 16)`.

### Byte Slice Support (for Python)

- **FR-11**: Support subscribing to a slice-based service (`Burst<Vec<u8>>`).
- **FR-12**: Support publishing variable-sized byte payloads with a configurable max slice length (initial limit).
- **FR-13**: Provide a way to configure publisher-side service settings (notably `history_size`) so IPC participants can match the same service contract across processes/languages.

Defaults (documented for interop predictability):
- Default `history_size`: `5`
- Default `subscriber_max_buffer_size`: `max(history_size, 16)`
- Default `initial_max_slice_len`: `128 * 1024` bytes

## Non-Functional Requirements

- **NFR-1 (Latency)**: `Spin` mode prioritizes lowest possible latency over CPU usage.
- **NFR-2 (CPU)**: `Threaded` and `Signaled` modes must allow near-idle CPU usage when no data is flowing.
- **NFR-3 (Safety)**: `ZeroCopySend` payload requirements are documented clearly (repr(C), self-contained).
- **NFR-4 (Operability)**: Docker/shared-memory constraints are documented (e.g., `/dev/shm` sizing).
- **NFR-5 (Testability)**: Provide deterministic tests that do not require IPC by default.
- **NFR-6 (Thread Lifecycle)**: Background threads (Threaded/Signaled subscribers) must not outlive graph shutdown; they must be stopped and joined deterministically.
- **NFR-7 (Python Safety)**: Python bindings must not panic on allocation/conversion failures; invalid user inputs should raise a Python error or skip sending in a way that is observable.
  - In Wingfoil’s bindings, `.iceoryx2_pub(...)` raises `TypeError` when stream values are not `bytes` or `list[bytes]` (no silent data loss).
- **NFR-8 (Diagnosability)**: Errors must carry enough structured context to distinguish:
  - environment failures (shared memory, permissions)
  - transient startup races (event service open/create in Signaled mode)
  - service contract incompatibility (e.g., mismatched `history_size`)
  - Notes:
    - “Config mismatch” detection is best-effort; do not rely on exact upstream error strings.
    - The invariant is: errors include the service contract context (`service_name`, `variant`, `history_size`, derived buffers).

## Test Requirements (TDD)

### Unit Tests (Rust)

- Validate `FixedBytes<N>` behavior (truncation, empty input, exact fit).
- Validate invalid service names fail fast and return an error (do not assert on exact error strings).
- Validate `Iceoryx2SubOpts::default()` provides stable defaults (`history_size`, `Spin`, `Ipc`).

### Integration Tests (Rust)

Default (runs in normal `cargo test`):
- Local variant round-trip for each subscriber mode:
  - `Local + Spin`
  - `Local + Threaded`
  - `Local + Signaled`

Feature-gated or ignored by default (requires shared memory environment):
- `Ipc` round-trip (may require `/dev/shm` / permissions).
- Late-joiner history test using non-Wingfoil publisher then Wingfoil subscriber.
- Cross-process (“true IPC”) test that forks/spawns separate binaries (optional).

Note:
- When validating late-joiner history, ensure the test collects *all* items from each `Burst<T>`.
  Avoid `collapse()` in the history assertions since it keeps only the last item of a burst.

### E2E Tests (Python)

Default (runs in `pytest` without IPC):
- `Local` pub/sub with `Spin` and `Threaded` modes using byte slices.

Optional / feature-gated:
- `Ipc` pub/sub between two Python processes (or Python + Rust) when shared memory is available.
- Large payload slice test to validate `initial_max_slice_len` behavior and error surface.

## Acceptance Criteria

- A developer can run **all default tests** on a laptop/CI runner without requiring IPC shared memory.
- A developer can enable an explicit feature (or run ignored tests) to validate IPC behavior in a suitable environment.
- Documentation clearly explains:
  - `Ipc` vs `Local` semantics
  - mode trade-offs (latency vs CPU)
  - history/buffering behavior
  - `update_connections()` rationale
  - why `always_callback()` is required for subscribers (they must be scheduled even without upstream data)
  - shared memory deployment constraints
- “Production readiness” guidance is explicit about what is (and is not) guaranteed by the beta adapter:
  - no durability/persistence guarantees (in-memory only)
  - Linux-first shared memory constraints
  - service contract coupling across languages/processes
- Development ergonomics:
  - A normal `git push` should be able to run pre-push hooks reliably (avoid persistent flakes in unrelated test suites).

Tracking:
- PR review closure work (CI + reviewer blockers) is tracked in `docs/plans/2026-04-05-003-iceoryx2-pr176-comment-closure-plan.md`.
