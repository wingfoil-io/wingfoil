# Plan: Enforce Advancing Time Between Cycles (Issue #130)

## Problem

`Graph` does not enforce that `state.time` advances between engine cycles. In
`process_callbacks_historical`, the cycle time is set directly to the next
scheduled callback time:

```rust
// graph.rs:596
self.state.time = self.state.next_scheduled_time();
```

If two callbacks share the same timestamp, both will be processed on separate
engine cycles at the **same** `time` value — violating the assumption held by
downstream nodes that time is strictly monotonic.

In `process_callbacks_realtime`, `NanoTime::now()` is used (line 623), which
is naturally monotonic in practice but is not explicitly guarded.

## Fix

### 1. `process_callbacks_historical` — enforce `>= prev + 1ns`

```rust
fn process_callbacks_historical(&mut self) -> bool {
    if !self.state.ready_callbacks.is_empty() {
        panic!("ready_callbacks are not supported in realtime mode.");
    }
    if self.state.has_scheduled_callbacks() {
        let next = self.state.next_scheduled_time();
        // Enforce strict monotonic progression: bump to prev+1 if needed.
        self.state.time = next.max(self.state.time + 1);
    }
    self.process_scheduled_callbacks()
}
```

### 2. `process_callbacks_realtime` — apply same guard

```rust
self.state.time = NanoTime::now().max(self.state.time + 1);
```

This is a safety net for clock resolution edge cases (e.g., two callbacks
queued within the same nanosecond on some platforms).

## Files to Change

- `wingfoil/src/graph.rs`
  - `process_callbacks_historical` (~line 591)
  - `process_callbacks_realtime` (~line 623)

## Tests to Add

In `wingfoil/src/graph.rs` (or an existing test module):

1. **Historical mode — duplicate timestamps**: schedule two callbacks at
   exactly the same `NanoTime`. Assert that each callback fires on a separate
   cycle with a strictly increasing `state.time`.

2. **Historical mode — past timestamp**: schedule a callback with a timestamp
   earlier than the current `state.time`. Assert that `state.time` is bumped
   to at least `prev_time + 1`, not set backwards.

## Acceptance Criteria

- `cargo test -p wingfoil` passes with the new tests.
- `cargo clippy --workspace --all-targets --all-features` passes cleanly.
- `cargo fmt --all` produces no diffs.
