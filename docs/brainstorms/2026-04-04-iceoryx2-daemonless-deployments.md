---
date: 2026-04-04
topic: iceoryx2-daemonless-deployments
---

# iceoryx2 Daemonless Deployments

## What We’re Solving

We want wingfoil’s `iceoryx2` adapter to work in production environments where we cannot rely on a central daemon (and where “run this daemon first” is operationally undesirable). We also want a simple story for intra-process (thread-to-thread) usage, primarily for tests and for embedding.

## Key Finding (Research)

iceoryx2 is decentralized: basic pub/sub does not require a central daemon. Service discovery and resources are backed by files in shared memory locations (commonly `/dev/shm` or `/tmp/iceoryx2`).

The FAQ also notes an important behavioral constraint: iceoryx2 does not run background threads; connection establishment can require explicit calls to `update_connections()` to avoid “publisher sent then exited” style drops.

## Approaches

### Approach A: Keep IPC Only, Fix Lifecycle and Connections (Recommended)

Keep the adapter defaulting to `ipc::Service` but ensure we manage connections and lifetimes correctly.

Pros:
- Preserves the intended inter-process semantics.
- No extra public API surface.

Cons:
- Requires careful lifecycle handling in wingfoil nodes (start/stop, no leaked threads).
- More sensitive to deployment environment (shared memory size, permissions).

### Approach B: Expose Service Variants (ipc vs local)

Add a small config or additional constructors to select `ipc::Service` vs `local::Service` (and optionally the `*_threadsafe` variants).

Pros:
- Enables daemonless + easy CI smoke tests via `local::Service`.
- Makes it explicit when users are using intra-process vs inter-process transport.

Cons:
- Adds API surface (but it is behind a `*-beta` feature).

### Approach C: Two APIs (iceoryx2_ipc_* and iceoryx2_local_*)

Provide separate functions instead of a config enum.

Pros:
- Very explicit.

Cons:
- Duplicates API and docs; risk of divergence.

## Recommendation

Implement Approach B with a minimal enum:

```rust
pub enum Iceoryx2ServiceVariant {
    Ipc,
    Local,
}
```

and add `iceoryx2_sub_with(...)` / `iceoryx2_pub_with(...)` constructors while keeping existing `iceoryx2_sub` / `iceoryx2_pub` defaulting to `Ipc`.

This keeps the “easy path” and gives us a daemonless test and embed story.

## Acceptance Criteria

- It is possible to use the adapter without any external daemon process.
- We have at least one deterministic test using `local::Service` that demonstrates end-to-end pub/sub inside a single process.
- Docs clearly explain:
  - `ipc` vs `local` semantics
  - shared memory deployment constraints (Docker `/dev/shm` sizing)
  - need for `update_connections()` in short-lived pub/sub setups

## Open Questions

- Should wingfoil call `update_connections()` only once on `start()`, or periodically (e.g., every N cycles) to handle late subscribers?
- Should we remove the background thread in `iceoryx2_sub` to align with wingfoil lifecycle and reduce latency?
