---
date: 2026-04-05
topic: iceoryx2-ipc-production-readiness
status: active
---

# iceoryx2 IPC — Production Readiness Brainstorm

## Problem Frame

Wingfoil’s `iceoryx2` adapter is currently behind `iceoryx2-beta` and is already usable, but IPC deployments are sensitive to:

- **Service-level configuration coupling** (e.g. `history_size`) across publisher/subscriber and across languages.
- **Short-lived graphs** where ports may not connect before shutdown without explicit connection updates.
- **Signaled mode** races around the event service (`"{service_name}.signal"`).
- **Operational diagnosis**: failures must be explainable quickly without reading source or iceoryx2 internals.

This brainstorm defines “production readiness” for IPC and identifies the highest-leverage next steps.

## Goals

- Make IPC behavior **predictable and configurable** (service contract is explicit, not implicit).
- Ensure IPC works reliably in **typical production lifecycles**:
  - late subscriber joins
  - short-lived publisher/subscriber processes
  - process restart / redeploy
  - containerized environments
- Improve **operability**:
  - errors carry enough context to diagnose contract mismatch vs environment issues
  - doc page tells operators what to check first
- Provide a **safe default test matrix** (Local default; IPC opt-in).

## Non-Goals

- Guarantee message delivery across process crashes (iceoryx2 is not durability/persistence).
- Build a full service discovery/registry management layer on top of iceoryx2.
- Support arbitrary-size payloads without explicit sizing constraints.

## Key Constraints / Realities

### Service Contract Coupling

`history_size` and derived buffer sizing are publish/subscribe **service-level** settings in iceoryx2.
All participants must agree, otherwise `open_or_create()` can fail.

### Performance vs CPU

We support three receive modes:
- `Spin`: lowest latency, highest CPU
- `Threaded`: channel-hop latency, reduced graph-thread load
- `Signaled`: event-driven wake, lowest idle CPU, can be µs-level

## Approach Options

### A) “Contract-first API” (Recommended)

Introduce a shared “contract” concept and ensure all constructors accept/derive it deterministically.

Pros:
- Eliminates silent mismatch footguns.
- Makes cross-language deployments explicit.
- Better docs and on-call clarity.

Cons:
- Slightly more API surface.

### B) “Hidden defaults” (Not Recommended)

Keep publisher config implicit and hope defaults are “good enough”.

Pros:
- Minimal API.

Cons:
- Fragile IPC; hard to diagnose; breaks cross-language usage.

### C) “Auto-negotiate” (Risky / likely not feasible)

Attempt to read service config from an existing service and adapt.

Pros:
- Nice UX if feasible.

Cons:
- Depends on iceoryx2 APIs; risks subtle compatibility issues; increases complexity.

## Recommended Direction

Proceed with **Approach A**:
- A contract helper (`Iceoryx2ServiceContract` / `Iceoryx2SliceContract`) becomes the canonical derived configuration.
- Publisher/subscriber options expose `contract()` so application code can compare/ensure consistency.
- Error types explicitly identify “config mismatch” vs “open failed” (best-effort).

## Success Criteria

- A production operator can answer: “Why is IPC failing?” by looking at the error + docs.
- A Python publisher can reliably interop with a Rust subscriber by setting the same contract values.
- There is at least one deterministic test that fails on contract mismatch and asserts typed context.

## Scope Boundaries

- Keep everything behind `iceoryx2-beta` until confidence is high.
- Do not change default behavior for `iceoryx2_sub()` / `iceoryx2_pub()` except via safer defaults and better errors.

