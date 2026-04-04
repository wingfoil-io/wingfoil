# iceoryx2 Adapter Implementation Plan

## Current Status: IN PROGRESS

## Completed Work

### Implementation
- [x] Added `iceoryx2-beta` feature flag to Cargo.toml
- [x] Created adapter module with `iceoryx2_sub()` and `iceoryx2_pub()`
- [x] Implemented Iceoryx2ReceiverStream for receiving samples
- [x] Implemented Iceoryx2Publisher with zero-copy loan-write-send pattern
- [x] Added Burst operation benchmarks in benches/iceoryx2.rs

### Testing
- [x] Added unit tests for Burst operations
- [x] Verified 95/96 tests pass (1 pre-existing zmq failure)

### Documentation
- [x] Created SPEC.md with comprehensive iceoryx2 documentation
- [x] Documented zero-copy requirements (ZeroCopySend, repr(C))
- [x] Documented deployment notes (shared memory locations, decentralized discovery)
- [x] Documented known issues (zmq flakiness, benchmark runtime)

### Cleanup
- [x] Fixed unused import/variable warnings in iceoryx2 module

## Remaining Work

### Research: Daemonless Deployments (COMPLETED)
- [x] Confirm daemonless IPC behavior end-to-end (no RouDi/iox-roudi required) using upstream docs/examples
- [x] Identify required connection management (`update_connections()`) points for pub/sub and how often to call them
- [x] Validate container deployment requirements: `/dev/shm` sizing, alternate SHM paths (`/tmp/iceoryx2`), user/permissions
- [x] Summarize findings in a short design note under `docs/brainstorms/`

## Cleanup & Commit Plan

Goal: clean up the worktree and commit changes using semantic, atomic commits.

### 1. Document Findings & Plan
- [x] Create `docs/brainstorms/2026-04-04-iceoryx2-daemonless-deployments.md`
- [x] Update `SPEC.md` with daemonless requirements, `Iceoryx2Mode`, and new API
- [x] Update `plan.md` with the new roadmap

### 2. Refactor iceoryx2 Adapter (Atomic Commits) (COMPLETED)
- [x] **Commit 1 (docs):** Add daemonless deployment brainstorm and update SPEC.md
- [x] **Commit 2 (feat):** Add `Iceoryx2ServiceVariant`, `Iceoryx2Mode`, API constructors, and tests
- [x] **Commit 3 (refactor):** Remove background thread from default `Spin` mode and implement polling in `read.rs`
- [x] **Commit 4 (refactor):** Implement periodic connection updates in `write.rs`
- [x] **Commit 5 (test/fix):** Apply clippy fixes and final polish

### 3. Address PR 176 Feedback (COMPLETED)
- [x] **Commit 6 (feat):** Implement `Iceoryx2Mode::Threaded` using high-frequency yield (10µs)
- [x] **Commit 7 (test):** Add round-trip integration tests for both `Spin` and `Threaded` modes
- [x] **Commit 8 (feat):** Add Python bindings for iceoryx2 adapter
- [x] **Commit 9 (fix):** Final documentation review and PR wrap-up

### 4. Fix GitHub Workflow Errors (COMPLETED)
- [x] Fix Rust formatting errors using `cargo fmt`
- [x] Fix clippy warnings and compilation errors in integration tests
- [x] Fix discovery and polling issues in `Threaded` mode (added `update_connections()` and `always_callback()`)
- [x] Verify local build and tests pass before final commit

### 5. Verification (COMPLETED)
- [x] Run final test suite: `cargo test -p wingfoil --features iceoryx2-beta`
- [x] Verify clean clippy: `cargo clippy --workspace --all-targets --all-features`
- [x] Verify Python compilation (build check)

### 6. Resolve PR Conflicts (COMPLETED)
- [x] Fetch latest changes from `upstream/main`
- [x] Rebase `feat/iceoryx2-v2` onto `upstream/main`
- [x] Resolve conflicts in `wingfoil/src/adapters/mod.rs` (due to `csv` refactor and `iterator_stream` move)
- [x] Fix compilation errors in `wingfoil-python` by gating iceoryx2 bindings behind feature flag
- [x] Verify all tests pass after rebase

### 7. Advanced Features & Enhancements (COMPLETED)

Goal: Leverage iceoryx2's more powerful features like dynamic slices and event-driven notifications.

#### Unit 1: Dynamic Slice Adapters (Zero-Copy Variable Sized Payloads) (COMPLETED)
- [x] Implement `iceoryx2_sub_slice` returning `Stream<Burst<Vec<u8>>>`
- [x] Implement `iceoryx2_pub_slice` taking a byte stream
- [x] Support variable-sized `loan_slice_uninit` and `write_from_slice`

#### Unit 2: Signaled Polling Mode (0% CPU Idle) (COMPLETED)
- [x] Add `Iceoryx2Mode::Signaled` which uses `Listener` + `WaitSet` for true blocking
- [x] Implement internal Event service pairing (`<service>.signal`)
- [x] Update publisher to trigger signal after every send

#### Unit 3: Python Slice Migration (COMPLETED)
- [x] Update Python bindings to use slices by default (removing `FixedBytes` restriction)
- [x] Verify variable-sized transfers between Python processes

#### Phase 3: Polish (COMPLETED)
- [x] Improve error mapping for creation failures
- [x] Add performance comparison benchmarks (fixed vs slice)
