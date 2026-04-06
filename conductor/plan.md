# Wingfoil iceoryx2 Test Suite Update (TDD)

## Status: COMPLETED (2026-04-05)

This plan originally tracked bringing the `iceoryx2` adapter to “CI-safe by default” testing
coverage (Local variant), while keeping IPC tests opt-in.

Current canonical docs:
- Requirements: `docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`
- Design: `docs/design/2026-04-05-iceoryx2-adapter-design.md`
- Testing spec: `conductor/spec.md`

## Test Strategy (Implemented)

### 1. Unit Tests (Rust)
- **Error surface:** invalid service name fails fast; open/create failures preserve typed context.
- **FixedBytes:** truncation + edge cases.
- **Contracts:** `Iceoryx2*Opts::contract()` exposes deterministic service settings.

### 2. Integration Tests (Rust)
- **Local round-trip:** `Spin`, `Threaded`, and `Signaled` all deliver values.
- **Slice round-trip:** `[u8]` slice pub/sub works across all modes (Local).
- **Contract mismatch:** intentional `history_size` mismatch fails with `Iceoryx2Error` and typed context.
- **Multi-process IPC:** covered via opt-in ignored tests behind `iceoryx2-integration-test`.

### 3. E2E Tests (Python)
- **Local pub/sub:** deterministic, no shared memory dependency.
- **IPC pub/sub:** opt-in via `WINGFOIL_ICEORYX2_IPC_TESTS=1`.
- **Large payloads:** validated via configurable `initial_max_slice_len` (default `128 * 1024`).

## Implementation Steps (TDD) (Done)

### Phase 1: Unit Tests
- [x] Add invalid-service-name coverage (fails fast) to `wingfoil/src/adapters/iceoryx2/mod.rs`.
- [x] Add `FixedBytes` edge-case coverage to `wingfoil/src/adapters/iceoryx2/mod.rs`.
- [x] Add contract derivation assertions to `wingfoil/src/adapters/iceoryx2/mod.rs`.

### Phase 2: Integration Tests
- [x] Add Local round-trip tests to `wingfoil/src/adapters/iceoryx2/integration_tests.rs`.
- [x] Add Local contract mismatch test to `wingfoil/src/adapters/iceoryx2/integration_tests.rs`.
- [x] Add IPC tests gated behind `iceoryx2-integration-test` and `#[ignore]`.

### Phase 3: Python E2E Tests
- [x] Create `wingfoil-python/tests/test_iceoryx2.py`.
- [x] Implement Local pub/sub coverage (modes + bytes/slices).
- [x] Implement IPC opt-in subprocess test via `WINGFOIL_ICEORYX2_IPC_TESTS=1`.
- [x] Implement large-payload slice coverage (configurable `initial_max_slice_len`).

## Verification
- Rust (default): `cargo test -p wingfoil --features iceoryx2-beta`
- Rust (IPC opt-in): `cargo test -p wingfoil --features iceoryx2-beta,iceoryx2-integration-test -- --ignored`
- Python (default): `cd wingfoil-python && uv run pytest -q`
- Python (IPC opt-in): `cd wingfoil-python && WINGFOIL_ICEORYX2_IPC_TESTS=1 uv run pytest -q`
