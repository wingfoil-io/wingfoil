---
date: 2026-04-05
topic: iceoryx2-ipc-production-hardening
status: completed
execution_posture: tdd
completed: 2026-04-05
---

# Plan: iceoryx2 IPC Production Hardening

Source context:
- `docs/brainstorms/2026-04-05-iceoryx2-ipc-production-readiness.md`
- `docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`
- `docs/design/2026-04-05-iceoryx2-adapter-design.md`

## Problem Frame

The `iceoryx2` adapter is production-targeted for IPC deployments but requires hardening around:
- service-level contract coupling (history/buffers/slice sizes)
- process lifecycle edge cases and connection updates
- diagnosability and reliable error classification

## Implementation Units

### Unit 1: Contract API (Rust)

Files:
- `wingfoil/src/adapters/iceoryx2/mod.rs`

Work:
- Define contract structs (`Iceoryx2ServiceContract`, `Iceoryx2SliceContract`).
- Expose deterministic contract derivation:
  - `Iceoryx2SubOpts::contract()`
  - `Iceoryx2PubOpts::contract()`
  - `Iceoryx2PubSliceOpts::contract()`

Test scenarios:
- Unit tests asserting derived contracts match expected defaults and custom settings.

### Unit 2: Contract Enforcement via Errors (Rust)

Files:
- `wingfoil/src/adapters/iceoryx2/mod.rs`
- `wingfoil/src/adapters/iceoryx2/read.rs`
- `wingfoil/src/adapters/iceoryx2/write.rs`

Work:
- Ensure service open/create failures carry typed context:
  - `Iceoryx2Error::ServiceOpenFailed { .. }`
  - `Iceoryx2Error::ServiceConfigMismatch { .. }` (best-effort classification)

Test scenarios:
- Integration test that intentionally mismatches publisher/subscriber contract and asserts:
  - error is `Iceoryx2Error`
  - typed context matches the attempted contract
  - variant is correct (Local/Ipc)

### Unit 3: Python Contract Interop

Files:
- `wingfoil-python/src/py_stream.rs`
- `wingfoil-python/src/py_iceoryx2.rs`
- `wingfoil-python/tests/test_iceoryx2.py`

Work:
- Expose publisher contract knobs:
  - `Stream.iceoryx2_pub(..., history_size=..., initial_max_slice_len=...)`
- Ensure the opt-in IPC E2E test sets matching publisher/subscriber history size explicitly.

Test scenarios:
- Default Local E2E stays deterministic.
- IPC E2E remains opt-in via env var and passes when enabled in an IPC-capable environment.

### Unit 4: CI / Guardrails

Files:
- `.github/workflows/rust.yml`
- `wingfoil/Cargo.toml` (bench feature gating)

Work:
- Ensure CI clippy runs with `-D warnings` for both:
  - `--all-targets`
  - `--all-targets --all-features`
- Ensure benches and IPC-gated code compile under both configurations.

## Commands

Rust (default):
- `cargo test -p wingfoil --features iceoryx2-beta`

Rust (CI-grade clippy):
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Python (uv-only, no pip):
- `cd wingfoil-python && uv run pytest -q`

Python IPC opt-in:
- `cd wingfoil-python && WINGFOIL_ICEORYX2_IPC_TESTS=1 uv run pytest -q`

## Risks

- iceoryx2 error types are not currently preserved end-to-end; mismatch classification is best-effort.
- CI runners may not support IPC by default; tests must remain opt-in.

## Result (Implemented)

The core “contract-first” hardening described here is implemented in the current adapter:
- Contracts: `Iceoryx2ServiceContract` / `Iceoryx2SliceContract`
- Deterministic derivation: `Iceoryx2*Opts::contract()`
- Typed errors with context: `Iceoryx2Error::ServiceOpenFailed { .. }` and best-effort `Iceoryx2Error::ServiceConfigMismatch { .. }`
- Python interop knobs: `Stream.iceoryx2_pub(..., history_size=..., initial_max_slice_len=...)`

Follow-up work (if any) should be tracked in a new plan file rather than extending this one.
