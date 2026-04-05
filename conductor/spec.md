# Wingfoil iceoryx2 Testing Specification

This document defines the intended test coverage for Wingfoil’s `iceoryx2` adapter.
It is intentionally aligned to the current implementation (contracts, modes, error types, and gating).

Source-of-truth code:
- `wingfoil/src/adapters/iceoryx2/mod.rs`
- `wingfoil/src/adapters/iceoryx2/read.rs`
- `wingfoil/src/adapters/iceoryx2/write.rs`
- `wingfoil/src/adapters/iceoryx2/integration_tests.rs`
- `wingfoil-python/src/py_iceoryx2.rs`
- `wingfoil-python/src/py_stream.rs`
- `wingfoil-python/tests/test_iceoryx2.py`

## Overview

We test two transport variants:
- `Local` (heap-backed): deterministic, daemonless, safe to run in CI by default.
- `Ipc` (shared memory): production semantics; tests are opt-in and environment-dependent.

We test three subscriber modes:
- `Spin`: receive inside the Wingfoil graph thread `cycle()`.
- `Threaded`: background thread receive + channel hop into graph thread.
- `Signaled`: background thread blocking WaitSet wake via `"{service}.signal"` Event service.

## Unit Test Scenarios (Rust)

### Contracts & Defaults

- **Defaults:** `Iceoryx2SubOpts::default()` uses `Ipc + Spin` and a non-zero `history_size`.
- **Contract derivation:** `Iceoryx2SubOpts::contract()` and `Iceoryx2PubOpts::contract()` derive identical `Iceoryx2ServiceContract` for the same `history_size`.
- **Slice contract derivation:** `Iceoryx2PubSliceOpts::contract()` derives `Iceoryx2SliceContract` including `initial_max_slice_len`.

### Error Surface

We intentionally avoid asserting on exact error strings (iceoryx2 may change them across versions).

- **Invalid service name fails fast:** calling `iceoryx2_sub::<T>("")` and running a 1-cycle graph errors.
- **Service open/create context:** open/create failures should surface as `Iceoryx2Error::ServiceOpenFailed { .. }` or `Iceoryx2Error::ServiceConfigMismatch { .. }` and include:
  - `service_name`, `variant`, `history_size`, `subscriber_max_buffer_size`

### FixedBytes

- **Truncation:** `FixedBytes<N>::new(bytes)` truncates to `N`.
- **Zero-length:** handles empty input correctly.
- **As slice:** `as_slice()` returns exactly `len` bytes.

## Integration Test Scenarios (Rust)

Default (runs without shared memory / IPC):
- Local round-trip coverage for each mode:
  - `test_local_spin_round_trip`
  - `test_local_threaded_round_trip`
  - `test_local_signaled_round_trip`
- Local slice round-trip coverage for each mode:
  - `test_local_slice_spin_round_trip`
  - `test_local_slice_threaded_round_trip`
  - `test_local_slice_signaled_round_trip`
- Contract mismatch surfaces error + typed context:
  - `test_local_service_config_mismatch_fails`

Opt-in (requires a shared memory capable environment):
- IPC round-trip coverage (ignored / feature gated):
  - `test_ipc_round_trip` (`#[ignore]` and gated behind `iceoryx2-integration-test`)
- IPC late-joiner behavior (ignored / feature gated):
  - `test_late_joiner_with_history` (`#[ignore]` and gated behind `iceoryx2-integration-test`)

## E2E Test Scenarios (Python)

Default (no IPC dependency):
- Local variant pub/sub in a single Python process:
  - bytes payloads, including `Burst`-like list handling
  - mode coverage includes at least `Spin`, `Threaded`, and `Signaled`

Opt-in (explicitly enabled):
- IPC cross-process pub/sub (Python subprocesses), enabled via:
  - `WINGFOIL_ICEORYX2_IPC_TESTS=1`
- Contract matching must be explicit in IPC tests (set `history_size` on publishers/subscribers).

## Verification Metrics

- **Default pass rate:** 100% for Local variant tests in CI.
- **IPC pass rate:** 100% when run in a compatible environment (Linux + shared memory + permissions).
- **Coverage target:** keep `wingfoil/src/adapters/iceoryx2/` highly exercised (aim for >90% line coverage when measured locally).
