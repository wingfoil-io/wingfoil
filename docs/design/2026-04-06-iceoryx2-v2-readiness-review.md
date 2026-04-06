---
date: 2026-04-06
topic: iceoryx2-v2-readiness-review
status: draft
---

# iceoryx2 v2 Branch — Readiness Review

Scope: current branch `feat/iceoryx2-v2` (PR #176) and the `iceoryx2-beta` adapter.

This is a “merge / deploy readiness” audit for the branch as it exists, not a proposal for new features.

## Executive Summary

The iceoryx2 adapter appears feature-complete for the stated beta scope:

- typed + slice pub/sub (`Burst<T>` and `Burst<Vec<u8>>`)
- `Ipc` and `Local` service variants
- subscriber modes: `Spin`, `Threaded`, `Signaled`
- explicit service contract helpers (`Iceoryx2ServiceContract`, `Iceoryx2SliceContract`)
- deterministic default tests using `Local` (IPC tests are opt-in)
- Python bindings for bytes / list-of-bytes with typed error mapping

Primary remaining risks are *operational / CI* rather than missing code:

- upstream PR workflows may be blocked behind maintainer approval (`action_required`)
- IPC tests depend on shared memory sizing/permissions and are intentionally gated

## CI State (PR #176)

As of **2026-04-06**, GitHub Actions runs on the upstream PR are present but not executed:

- `gh run list --repo wingfoil-io/wingfoil --branch feat/iceoryx2-v2` shows successful runs.
- Example run IDs:
  - 24029016531 (workflow: “CI”)
  - 24029016502 (workflow: “iceoryx2 (Local) Integration Tests”)

Operational impact:
- `gh pr checks` may still report “no checks” depending on repo settings; use `gh run list` as the source of truth for job execution.

## What’s Implemented (Evidence)

- Feature gating:
  - Rust: `wingfoil/Cargo.toml` (`iceoryx2-beta`, `iceoryx2-integration-test`)
  - Module gating: `wingfoil/src/adapters/mod.rs` exports adapter only under feature
- Adapter core:
  - Types + errors: `wingfoil/src/adapters/iceoryx2/mod.rs`
  - Subscriber: `wingfoil/src/adapters/iceoryx2/read.rs`
  - Publisher: `wingfoil/src/adapters/iceoryx2/write.rs`
- Tests:
  - Default Local round-trips + mismatch test: `wingfoil/src/adapters/iceoryx2/integration_tests.rs`
- Python bindings:
  - `iceoryx2_sub` (slice subscriber) + `.iceoryx2_pub()` (slice publisher): `wingfoil-python/src/py_iceoryx2.rs`, `wingfoil-python/src/py_stream.rs`

## Readiness Findings

### Blockers (Would Prevent Safe Landing)

- None found in the branch by static review alone.

Note: a “blocker” could still exist if `cargo test` or `cargo clippy` fails on the current head; re-run the checklist before merging.

### Friction (Agent/CI/Operator Cost)

- **CI approval gating**: PR workflows on upstream repo may remain `action_required` pending maintainer approval (non-code blocker; affects turnaround time).
- **IPC environment sensitivity**: true IPC tests require `/dev/shm` (or configured SHM path) with adequate size and permissions; these are intentionally opt-in.
- **Service mismatch detection is best-effort**: `ServiceConfigMismatch` vs `ServiceOpenFailed` is classified via heuristics; diagnostics should rely on included contract context, not error strings.

### Optimizations (Nice-to-Have)

- Consider logging `update_connections()` failures at `debug`/`trace` in `Spin` mode (currently ignored), if it materially improves field diagnosis without log spam.
- Consider an operator-facing “IPC runbook” page (symptoms → checks) if this adapter is expected to be deployed widely during beta.

## Pre-Merge Checklist

- Rust tests (default, no IPC dependency):
  - `cargo test -p wingfoil --features iceoryx2-beta`
- Rust tests (compile + feature-gated ignored IPC tests wired correctly):
  - `cargo test -p wingfoil --features iceoryx2-beta,iceoryx2-integration-test`
- Clippy (workspace):
  - `cargo clippy --workspace --all-targets --all-features`
- Optional IPC validation (requires shared memory environment; runs ignored tests):
  - `cargo test -p wingfoil --features iceoryx2-beta,iceoryx2-integration-test -- --ignored`
- Optional Python validation (environment-dependent):
  - `cd wingfoil-python && uv sync --extra dev --locked`
  - `cd wingfoil-python && uv run maturin develop`
  - `cd wingfoil-python && uv run pytest -q`

## Local Verification Snapshot (This Machine)

As of **2026-04-06**, the following commands were run successfully in this workspace:

- `cargo test -p wingfoil --features iceoryx2-beta`
- `cargo clippy --workspace --all-targets --all-features`
- `cargo test -p wingfoil --features iceoryx2-beta,iceoryx2-integration-test -- --ignored`
- `cd wingfoil-python && uv run pytest -q` (Local adapter tests pass; IPC subprocess test is skipped by default)

## Documentation Pointers

- Requirements: `docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`
- Design: `docs/design/2026-04-05-iceoryx2-adapter-design.md`
- Spec: `SPEC.md` (iceoryx2 adapter section)

## “Skills” Notes

- CLI agent-readiness review is not applicable in this repo snapshot (no user-facing CLI implementation; `conductor/` is documentation-only).
- Agent-native architecture audit is not applicable here unless Wingfoil is being embedded into an agent-driven app (this branch is a transport adapter + bindings).
