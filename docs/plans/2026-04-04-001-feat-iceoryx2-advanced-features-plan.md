---
title: "feat: Add advanced iceoryx2 features (Slices & Signaled mode)"
type: feat
status: completed
date: 2026-04-04
origin: conductor/plan.md
completed: 2026-04-05
---

# feat: Add advanced iceoryx2 features (Slices & Signaled mode)

## Overview
Enhance the iceoryx2 adapter with dynamic slice support for variable-sized payloads and a true event-driven notification mode (`Signaled`) for 0% CPU idle.

## Problem Frame
The current implementation is restricted to fixed-size types and uses a high-frequency yield loop for threaded subscribers, which is CPU-intensive even when no data is arriving. Python bindings are also currently limited by a hardcoded `FixedBytes` size.

## Requirements Trace
- R1. Support for variable-sized byte slices (`[u8]`) using iceoryx2's native slice loaning.
- R2. Integration of slice support into Python bindings to remove `FixedBytes` size restrictions.
- R3. Implementation of `Iceoryx2Mode::Signaled` using an internal `Event` service for zero-CPU wait.
- R4. Improved error mapping for iceoryx2 service and port creation.

## Scope Boundaries
- Signaled mode requires both publisher and subscriber to opt-in (as the publisher must trigger the signal).
- Slice support will be implemented initially for `[u8]` (byte streams).

## Context & Research
- `iceoryx2 0.8` supports `loan_slice_uninit` and `write_from_slice`.
- `WaitSet` requires a `Listener` from an `Event` service for notifications in v0.8.
- Default signal service name: `<data_service_name>.signal`.

## Implementation Units

### Unit 1: Dynamic Slice Adapters
**Goal:** Implement slice-based pub/sub in Rust.
**Files:**
- Modify: `wingfoil/src/adapters/iceoryx2/read.rs`
- Modify: `wingfoil/src/adapters/iceoryx2/write.rs`
- Modify: `wingfoil/src/adapters/iceoryx2/mod.rs`
**Approach:**
- Add `iceoryx2_sub_slice()` and `iceoryx2_pub_slice()` using `<[u8]>` as the payload type.
- Update `Iceoryx2ReceiverStream` and `Iceoryx2Publisher` to handle slice loans.
**Test Scenarios:**
- Happy path: Send `[1, 2, 3]`, receive `[1, 2, 3]`.
- Happy path: Send `[4, 5, 6, 7]`, receive `[4, 5, 6, 7]` (variable size).

### Unit 2: Signaled Polling Mode
**Goal:** Implement event-driven notifications.
**Files:**
- Modify: `wingfoil/src/adapters/iceoryx2/read.rs`
- Modify: `wingfoil/src/adapters/iceoryx2/write.rs`
- Modify: `wingfoil/src/adapters/iceoryx2/mod.rs`
**Approach:**
- Add `Iceoryx2Mode::Signaled`.
- Publisher: Create a `Notifier` for `<service>.signal`. Trigger it after every `send()`.
- Subscriber: Create a `Listener` for `<service>.signal`. Attach to `WaitSet` in the background thread.
**Test Scenarios:**
- Happy path: Receive samples correctly in `Signaled` mode.
- Integration: Verify 0% CPU usage (via manual check or timing assertions) while idle.

### Unit 3: Python Slice Migration
**Goal:** Update Python bindings to use dynamic slices.
**Files:**
- Modify: `wingfoil-python/src/py_iceoryx2.rs`
- Modify: `wingfoil-python/examples/iceoryx2_pubsub.py`
**Approach:**
- Refactor `py_iceoryx2_sub` and `py_iceoryx2_pub_inner` to use the new slice-based nodes.
- Remove `FixedBytes` dependency in Python flow.
**Test Scenarios:**
- Python example correctly transfers bytes of various sizes.

## Verification
- `cargo test -p wingfoil --features iceoryx2-beta`
- Manual verification of CPU usage in `Signaled` mode.

## Result (Implemented)

This plan is implemented on this branch. The current design + requirements are captured in:
- `docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`
- `docs/design/2026-04-05-iceoryx2-adapter-design.md`

Test coverage is codified in:
- `wingfoil/src/adapters/iceoryx2/mod.rs`
- `wingfoil/src/adapters/iceoryx2/integration_tests.rs`
- `wingfoil-python/tests/test_iceoryx2.py`
