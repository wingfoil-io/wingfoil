# iceoryx2 Adapter

Zero-copy inter-process communication (IPC) via shared memory using iceoryx2.

## Module Structure

```
iceoryx2/
  mod.rs               # Config types (ServiceContract, SubOpts, PubOpts, Mode, Variant),
                       #   FixedBytes<N>, Iceoryx2Error, unit tests
  read.rs              # iceoryx2_sub*, Iceoryx2ReceiverStream — subscriber (producer)
  write.rs             # iceoryx2_pub* — publisher (consumer)
  local_tests.rs       # in-process Local-variant tests (run with default test suite)
  integration_tests.rs # gated by iceoryx2-integration-test (cross-process IPC tests)
```

## Key Design Decisions

### Three Polling Modes

The subscriber supports three modes via `Iceoryx2Mode`:

- **Spin** (default) — polls directly inside `cycle()` with `always_callback()`. Lowest latency (~1–5 µs), highest CPU.
- **Threaded** — background thread polls with 10 µs yield, delivers via channel. Lower CPU, ~10–100 µs added latency.
- **Signaled** — event-driven WaitSet (true blocking). Requires publisher to signal on a matching Event service.

### Service Contracts

All participants on the same service must use compatible `Iceoryx2ServiceContract` settings (history_size, subscriber_max_buffer_size). Mismatches produce `Iceoryx2Error::ServiceConfigMismatch`.

### Typed vs Slice APIs

- **Typed** (`iceoryx2_sub<T>` / `iceoryx2_pub<T>`) — `T` must be `#[repr(C)]`, `ZeroCopySend`, and self-contained (no heap pointers).
- **Slice** (`iceoryx2_sub_slice` / `iceoryx2_pub_slice`) — transfers `Vec<u8>` via `[u8]` shared memory slices. Used by Python bindings via `FixedBytes<N>`.

### FixedBytes<N>

A `#[repr(C)]` fixed-size byte buffer implementing `ZeroCopySend`. Bridges the gap between variable-length Python bytes and iceoryx2's fixed-layout shared memory.

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --exclude wingfoil-python -- -D warnings
cargo test -p wingfoil --features iceoryx2-beta
cargo test -p wingfoil --features iceoryx2-integration-test -- --test-threads=1
```

## Gotchas

- Feature flag is `iceoryx2-beta` (not `iceoryx2`) because the iceoryx2 crate is pre-1.0.
- Shared memory paths on Linux live under `/dev/shm/`; stale files from crashed processes may need manual cleanup.
- Service names must be non-empty and follow iceoryx2's naming rules — invalid names fail fast in `start()`.
