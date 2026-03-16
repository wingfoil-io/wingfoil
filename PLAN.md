# Fix Unwrap: Replace `.unwrap()` / `.expect()` Calls (Issue #11)

## Overview

Branch: `fix-unwrap` | Working dir: `/home/jake/wingfoil/dev04`

The codebase already uses `anyhow` throughout and all `MutableNode` lifecycle methods return `anyhow::Result`. No new error types or trait signature changes are needed beyond those listed below. The task is replacing panicking calls with `?`-propagation inside existing `Result`-returning contexts.

---

## Public API Changes Required

| Method | Current Return | Proposed Return |
|--------|---------------|-----------------|
| `GraphState::ready_notifier()` | `ReadyNotifier` | `anyhow::Result<ReadyNotifier>` |
| `GraphState::add_callback()` | `()` | `anyhow::Result<()>` |
| `GraphState::current_node_id()` | `usize` | `anyhow::Result<usize>` |
| `GraphState::always_callback()` | `()` | `anyhow::Result<()>` |
| `GraphState::ticked()` | `bool` | `anyhow::Result<bool>` |
| `FeedbackSink::send()` | `()` | `anyhow::Result<()>` |
| `CsvOperators::csv_write()` | `Rc<dyn Node>` | `anyhow::Result<Rc<dyn Node>>` |
| `CsvVecOperators::csv_write_vec()` | `Rc<dyn Node>` | `anyhow::Result<Rc<dyn Node>>` |

---

## Implementation Steps

### Step 1 — Low-friction infallibles (no API change)
**Files:** `queue/time_queue.rs` (lines 21, 31), `graph.rs` (lines 252, 511, 548), `nodes/demux.rs` (lines 107, 131, 443), `time.rs` (lines 100, 120)

Replace `.unwrap()` with `.expect("invariant: ...")`. No callers change. Zero API impact.

Examples:
- `time_queue.rs:21` → `.expect("TimeQueue::next_time() called on empty queue")`
- `time_queue.rs:31` → `.expect("TimeQueue::pop() called on empty queue")`
- `graph.rs:252` → `.expect("crossbeam select guarantees message is ready")`
- `graph.rs:511` → `.map_err(|_| anyhow::anyhow!("layer index overflows i32"))?` (in `initialise()`)
- `graph.rs:548` → `.expect("node seen but not indexed — internal inconsistency")`
- `demux.rs:107` → `.expect("index must be in available set")`
- `demux.rs:131` → `.expect("overflow stream must be set by demux constructor")`
- `demux.rs:443` → `.ok_or_else(|| anyhow::anyhow!("demux index out of bounds"))?`
- `time.rs:100` → `.expect("timestamp out of nanosecond range (valid ~1677–2262)")`
- `time.rs:120` → `.expect("timestamp out of nanosecond range (valid ~1677–2262)")`
- `adapters/iterator_stream.rs:33` → `.ok_or_else(|| anyhow::anyhow!("iterator exhausted unexpectedly"))?` (in `IteratorStream::cycle()`)
- `adapters/iterator_stream.rs:84` → `.ok_or_else(|| anyhow::anyhow!("iterator exhausted unexpectedly"))?` (in `SimpleIteratorStream::cycle()`)
- `nodes/channel.rs:173` → `.expect("batch not empty (checked above)")` (guarded by `is_empty` check)
- `nodes/channel.rs:201` → `.expect("queue front exists (checked above)")` (guarded by prior check)
- `nodes/channel.rs:210` → `.expect("queue front exists (invariant)")` (guarded by prior check)

### Step 2 — `GraphState::current_node_index` helper and cascade
**File:** `graph.rs` lines 161, 188, 192, 196, 213

Add private helper:
```rust
fn current_node_index_checked(&self) -> anyhow::Result<usize> {
    self.current_node_index
        .ok_or_else(|| anyhow::anyhow!("current_node_index not set — called outside node lifecycle"))
}
```

Change `ready_notifier()`, `add_callback()`, `current_node_id()`, `always_callback()`, `ticked()` to return `Result`. Then add `?` at every call site in:
- `tick.rs`, `feedback.rs`, `channel.rs`, `always.rs`, `graph_node.rs`, `receiver.rs`, `async_io.rs`, `zmq.rs` (start/setup/cycle methods)

This is the largest cascading change — mechanical but widespread.

### Step 3 — `FeedbackSink::send()` return type
**File:** `nodes/feedback.rs` line 71

Change `send()` to `-> anyhow::Result<()>`:
```rust
// Before
self.node_id.get().unwrap()
// After
self.node_id.get()
    .ok_or_else(|| anyhow::anyhow!("FeedbackSink used before setup — call send() only from cycle()"))?
```

Update all `cycle` call sites to use `?`.

### Step 4 — `OnceCell` unwraps in `graph_node.rs`
**File:** `nodes/graph_node.rs` lines 60, 75, 97, 117, 192, 237, 353

Replace `get_mut().unwrap()` / `set(...).unwrap()` with `ok_or_else` / `map_err`:
```rust
// get_mut
self.receiver_stream.get_mut()
    .ok_or_else(|| anyhow::anyhow!("receiver_stream not initialized"))?

// set
self.receiver_stream.set(receiver_stream)
    .map_err(|_| anyhow::anyhow!("receiver_stream already set — setup called twice?"))?;
```

All are within `anyhow::Result`-returning methods already.

### Step 5 — Thread join unwraps
**Files:** `nodes/graph_node.rs` lines 83, 102, 225, 257; `bencher.rs` line 79

In `teardown` (lines 102, 257 — already return `Result`):
```rust
handle.join()
    .map_err(|_| anyhow::anyhow!("worker thread panicked"))?;
```

In spawned task closures (lines 83, 225 — closures return `()`):
```rust
if let Err(e) = graph.run() {
    eprintln!("graph error in worker thread: {e:?}");
}
```

Or: convert to `move || -> anyhow::Result<()>` and collect `JoinHandle<anyhow::Result<()>>`, checked in `teardown`.

### Step 6 — `graph.rs` crossbeam recv
**File:** `graph.rs` line 604

Change `process_ready_callbacks` to return `anyhow::Result<bool>`:
```rust
let ix = self.state.ready_callbacks.recv()
    .map_err(|e| anyhow::anyhow!("ready_callbacks channel closed: {e}"))?;
```

### Step 7 — CSV adapter factory functions
**File:** `adapters/csv_streams.rs` lines 24, 27, 98–99, 143, 160

- Change `csv_iterator` to `-> anyhow::Result<Box<dyn Iterator<...>>>`
- Update `csv_read` / `csv_read_vec` to return `anyhow::Result<Rc<dyn Stream<...>>>`
- Change `write_header` to `-> anyhow::Result<()>` and add `?` in `cycle`
- Change `csv_write` / `csv_write_vec` trait methods to return `anyhow::Result<Rc<dyn Node>>`

### Step 8 — Async/socket adapters
**Files:** `adapters/socket.rs` lines 71, 102; `channel/kanal_chan.rs` lines 169–177

- `JRPCWriter::send` → `async fn send(...) -> anyhow::Result<()>`
- `JRPCSocket::connect` → `anyhow::Result<Self>`
- `AsyncChannelSender::send_message` → `async fn ... -> anyhow::Result<()>`

### Step 9 — Remaining `graph_node.rs` recv_timeout
**File:** `nodes/graph_node.rs` line 233

```rust
let notifier = rx_notif.recv_timeout(timeout)
    .map_err(|_| anyhow::anyhow!("timed out waiting for worker graph notifier (100ms)"))?;
```

### Step 10 — `demux.rs` remaining setup unwraps
**File:** `nodes/demux.rs` lines 216, 408

- Line 216 (in `cycle`): `self.overflow_graph_index.ok_or_else(|| anyhow::anyhow!("demux overflow index not set — setup not called?"))?`
- Line 408 (in `setup`): `self.overflow_child.take().ok_or_else(|| anyhow::anyhow!("overflow_child already taken — setup called twice?"))?`

---

## Out of Scope

- `.unwrap()` inside `#[cfg(test)]` blocks — acceptable per issue
- Intentional `panic!()` guards (e.g., async context guard in `graph.rs:174`, `Overflow::panic()` in `demux.rs`)
- `bencher.rs:108` enum conversion guard — keep as `panic!`
- KDB read/write unwraps — all in `#[cfg(test)]`

---

## Pre-Commit Checklist (after each step)

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test -p wingfoil
```

Optional ratchet — add to each file once its unwraps are cleared:
```rust
#![warn(clippy::unwrap_used)]
```

---

## Critical Files

- `src/graph.rs` — highest-density unwraps; `GraphState` API changes land here
- `src/nodes/graph_node.rs` — `OnceCell`, thread join, `recv_timeout` unwraps
- `src/nodes/feedback.rs` — `FeedbackSink::send()` API change cascades widely
- `src/adapters/csv_streams.rs` — public factory API change affects examples
- `src/types.rs` — reference contract for `MutableNode`/`Node`/`GraphState` (no changes needed)
