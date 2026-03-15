# Graph Dynamism — Implementation Plan

## Issue
GitHub issue #54: Enable dynamic node addition/removal during graph execution.

## Motivation

The current `demux` pattern virtualises dynamic graphs by pre-allocating a fixed number of
slots at build time. This requires knowing maximum concurrency upfront and wastes resources
on idle slots.

Dynamic graph addition removes the pre-allocation requirement. The canonical use case is
the RFQ example: when a new instrument appears, wire in a fresh per-instrument processing
subgraph on demand; tear it down when the instrument is closed.

## Concrete Example

```
source (layer 0)                         ← emits (instrument, price)
  ├── filter_A (layer 1)
  │     └── process_A (layer 2)
  │           └── aggregation (layer 3)  ← also has source as upstream
  └── filter_B (layer 1)
        └── process_B (layer 2)
              └── aggregation (layer 3)
```

When a new instrument C arrives, `aggregation.cycle()` fires (source is an active upstream).
It builds `filter_C → process_C` and calls `group.insert(state, key, process_C)`.
`DynamicGroup::insert` calls `state.add_upstream` internally; the graph wires `process_C`
in at the end of the cycle and registers it as an active upstream of `aggregation`.
From then on, when `process_C` ticks, `aggregation` is marked dirty automatically.

The aggregation node maintains an internal `DynamicGroup<Instrument, ProcessedPrice>` to
track and peek at dynamic upstreams. It uses `group.ticked_iter(state)` in `cycle()` to
iterate only the streams that fired. `ticked()` returns `false` (not a panic) when called
with a node that is not yet registered — see `ticked()` guard below.

For most use cases the full `MutableNode` boilerplate can be avoided entirely by using
`dynamic_group_stream` — see the `dynamism` example.

## Scope

### In scope (this PR)
- `state.add_upstream(node, is_active)` — wire a subgraph into the graph and register it
  as an upstream of the calling node (active or passive).
- `state.remove_node(node)` — deregister a node at the end of the current cycle.
- Layer recalculation when a new upstream lands at the same layer or deeper than the
  calling node (see below).

### Out of scope
- Inserting a node between two existing nodes (requires mutating an existing node's
  declared upstreams, which is architecturally unsupported).

## Public API

### Low-level: new methods on `GraphState`

```rust
/// Wire `upstream` (and its upstream subgraph) into the graph and register it
/// as an upstream of the calling node. `is_active` controls whether it triggers
/// the calling node on each tick (true) or is read-only (false).
/// Processed at the end of the current cycle.
///
/// If `recycle` is true, `add_callback_for_node(new_node_index, state.time())`
/// is called on the new upstream (not the calling node) after it is wired,
/// scheduling it to fire on the very next engine iteration. This lets the
/// calling node catch the value that triggered the `add_upstream` call
/// without waiting for the next source tick.
pub fn add_upstream(&mut self, upstream: Rc<dyn Node>, is_active: bool, recycle: bool)

/// Deregister `node` at the end of the current cycle:
/// unlinks it from all upstream downstream-lists and all downstream upstream-lists,
/// then calls stop() + teardown().
pub fn remove_node(&mut self, node: Rc<dyn Node>)
```

### `ticked()` guard

`ticked()` must not panic when called with an unregistered node. This matters because
`DynamicGroup::insert` puts a stream into the backing store immediately but only queues
the graph wiring — so `ticked_iter` may call `ticked()` on a stream that isn't registered
yet in the same `cycle()`. Change the implementation from `.unwrap()` to returning `false`
for unknown nodes:

```rust
pub fn ticked(&self, node: Rc<dyn Node>) -> bool {
    self.node_index(node)
        .map(|i| self.node_ticked[i])
        .unwrap_or(false)
}
```

### High-level: `DynamicGroup` and `dynamic_group_stream`

Most users should not call `state.add_upstream` / `state.remove_node` directly. Two
higher-level abstractions are provided in `src/dynamic_group.rs`:

#### `StreamStore<K, T>` trait

Backing-store abstraction with three methods: `store_insert`, `store_remove`,
`store_entries`. Blanket implementations are provided for `BTreeMap` (requires `K: Ord`)
and `HashMap` (requires `K: Hash + Eq`). Custom stores can be implemented for other
collections.

#### `DynamicGroup<K, T, S = BTreeMap<...>>`

A keyed collection of dynamically-wired streams that keeps the backing store and graph
wiring in sync:

```rust
// Register a stream under a key — calls state.add_upstream internally
group.insert(state, key, stream);

// Remove a stream by key — calls state.remove_node internally
group.remove(state, &key);

// Iterate (key, value) pairs for streams that ticked this cycle
for (key, value) in group.ticked_iter(state) { ... }
```

Backed by `BTreeMap` by default; use `DynamicGroup::with_store(HashMap::new())` for a
hash-keyed store, or supply any `StreamStore` implementation.

#### `dynamic_group_stream` / `dynamic_group_stream_with_store`

A turn-key combinator that wraps the full dynamic-group pattern — add/remove lifecycle,
per-key factory, and output folding — into a single pipeline expression:

```rust
let stream = dynamic_group_stream(
    add_stream,          // Rc<dyn Stream<K>>: fires when a key is added
    del_stream,          // Rc<dyn Stream<K>>: fires when a key is removed
    |key| { ... },       // factory: K -> Rc<dyn Stream<T>>
    initial_value,       // V: initial output value
    |out, key, val| { }, // on_tick: called for each per-key stream that fired
    |out, key| { },      // on_remove: called when a key is deleted
);
```

Use `dynamic_group_stream_with_store` to supply a custom `StreamStore` backing.

## Implementation

### Data structure changes

**`NodeData`** — add `active: bool` flag:
```rust
struct NodeData {
    node: Rc<dyn Node>,
    upstreams: Vec<(usize, bool)>,
    downstreams: Vec<(usize, bool)>,
    layer: usize,
    active: bool,   // NEW — false after remove_node()
}
```

**`GraphState`** — add pending queues:
```rust
pub struct GraphState {
    // ... existing fields ...
    pending_additions: Vec<(Rc<dyn Node>, usize, bool, bool)>, // NEW
    //   ^ (node_to_add, calling_node_index, is_active, recycle)
    pending_removals: Vec<Rc<dyn Node>>,  // NEW
}
```

`pending_removals` stores `Rc<dyn Node>` (not indices) so that nodes queued for removal
in the same cycle they were added can be looked up correctly after `process_pending_additions`
resolves their indices.

### `apply_nodes` guard

`apply_nodes` (used for setup/start/stop/teardown) iterates `0..nodes.len()`. It must
skip inactive nodes to avoid double-calling stop/teardown on nodes already processed by
`process_pending_removals`:

```rust
fn apply_nodes(&mut self, ...) -> anyhow::Result<()> {
    for ix in 0..self.state.nodes.len() {
        if !self.state.nodes[ix].active {
            continue;
        }
        // ... existing logic ...
    }
}
```

### Lifecycle at the cycle boundary

`Graph::cycle()` is updated to call the pending queues after `reset()`:

```rust
fn cycle(&mut self) -> anyhow::Result<()> {
    for lyr in 0..self.state.dirty_nodes_by_layer.len() {
        for i in 0..self.state.dirty_nodes_by_layer[lyr].len() {
            let ix = self.state.dirty_nodes_by_layer[lyr][i];
            self.cycle_node(ix)?;
        }
    }
    self.reset();
    self.process_pending_removals()?;
    self.process_pending_additions()?;
    Ok(())
}
```

Ordering: removals before additions (so a remove-then-re-add of the same node in one cycle
works correctly), and both after `reset()` so dirty structures are clean when `setup()`/
`start()` run and potentially call `add_callback`.

**`process_pending_removals()`**:
1. For each queued `Rc<dyn Node>`, resolve to index via `node_to_index`:
   a. Remove the node from all of its upstreams' `downstreams` lists.
   b. Remove the node from all of its downstreams' `upstreams` lists (so stale indices
      don't accumulate in callers' upstream lists and don't skew future `fix_layers` calls).
   c. Call `stop()` then `teardown()` (with `current_node_index` set appropriately).
   d. Set `NodeData.active = false`.

**`process_pending_additions()`**:
1. Record `start_index = nodes.len()`.
2. For each queued `(node, caller_index, is_active, recycle)`:
   a. `initialise_node(node)` — recurses through subgraph; `seen()` check prevents
      re-wiring already-present nodes. Returns the index for this node.
      `initialise_node` calls `push_node` internally, which pushes `false` to
      `node_ticked` and inserts into `node_to_index`.
3. Collect `new_indices`: all indices `>= start_index` (truly new nodes only).
4. For each new index: push `false` to `node_dirty` (mirrors how `initialise()` does it;
   `node_ticked` is already handled by `push_node` inside `initialise_node`).
5. For each new index: update its upstreams' `NodeData.downstreams`.
6. Extend `dirty_nodes_by_layer` if new max layer exceeds current vec length.
7. For each queued `(node, caller_index, is_active, recycle)`:
   a. Look up `node_index = node_to_index[node]` (may already have existed — use seen index).
   b. Add `(node_index, is_active)` to `nodes[caller_index].upstreams`.
   c. Add `(caller_index, is_active)` to `nodes[node_index].downstreams`.
      Note: steps b–c are always executed even when `node` was already registered (`seen()`
      returned true). `seen()` only prevents re-running `initialise_node` / setup / start;
      the caller→upstream edge is always a new relationship and must always be wired.
   d. Run `fix_layers(caller_index)` to recalculate layers if needed.
      `fix_layers` extends `dirty_nodes_by_layer` itself (see algorithm below) — no
      separate extension step is required here.
   e. If `recycle`: call `state.add_callback_for_node(node_index, state.time())` so the
      new upstream fires on the very next engine iteration.
8. **Batch setup**: call `setup()` on all new nodes in order.
9. **Batch start**: call `start()` on all new nodes in order (after all setup is done).

Steps 7–9 are ordered so that upstream relationships and layer assignments are finalised
before `start()` runs. This matters because `start()` may call `add_callback(state.time())`
and the node should be at its correct layer before its first tick.

### Layer fix algorithm

```
fix_layers(node_index):
    required = max(nodes[upstream_idx].layer for upstream_idx in nodes[node_index].upstreams) + 1
    if required > nodes[node_index].layer:
        nodes[node_index].layer = required
        for (downstream_idx, _) in nodes[node_index].downstreams:
            fix_layers(downstream_idx)
    extend dirty_nodes_by_layer if nodes[node_index].layer >= dirty_nodes_by_layer.len()
```

`dirty_nodes_by_layer` is a per-cycle scratch structure (cleared each cycle), so updating
`layer` on a node simply means it will slot into the correct bucket on the next cycle.
No data migration is needed.

`fix_layers` only ever **increases** a node's layer, never decreases it. Layer values are
therefore monotonically non-decreasing over the graph's lifetime. Calling `remove_node`
does not trigger a layer recalculation — the caller's layer may be higher than strictly
necessary after a removal, but this is correct (a higher layer just means the node cycles
later than strictly necessary) and avoids the complexity of layer reduction.

### `cycle_node` guard

```rust
fn cycle_node(&mut self, index: usize) -> anyhow::Result<()> {
    if !self.state.nodes[index].active {
        return Ok(());
    }
    // ... existing logic ...
}
```

## Timing: Deferred Wiring and the First-Tick Question

New nodes are always wired at the **end of the current engine cycle** — after all dirty
nodes for that cycle have run. This is required for safety: rewiring mid-cycle could
corrupt the dirty/downstream lists that the cycle loop is still iterating.

**Consequence**: the data value that caused a node to call `add_upstream` (e.g. the first
price for a new instrument) is processed by the calling node in the current cycle, but the
newly added subgraph misses it — it does not exist until the cycle ends.

**Opt-in first tick via `recycle: true`**: passing `recycle: true` to `add_upstream`
schedules a callback on the **new upstream node** (not the calling node) after it is wired.
Implementation: `process_pending_additions` calls `state.add_callback_for_node(new_node_index, state.time())`
for each addition that was queued with `recycle: true`. This fires the new upstream on the
very next engine iteration, at which point it reads its own upstream's `peek_value()` (which
still holds the triggering value if the source has not yet ticked again). Use this when the
calling node needs to process the value that caused the `add_upstream` call through the new
subgraph immediately.

Default behaviour (`recycle: false`) means the new subgraph begins processing from the
next upstream tick onward. This is acceptable when the calling node handles the triggering
value itself and only needs the subgraph for subsequent ticks.

Note: `add_callback(state.time())` behaves differently between run modes. In historical
mode the scheduled time is exact and fires on the very next engine iteration. In real-time
mode the time has already passed by the time wiring completes, so the callback fires
immediately on the next pass through the ready queue. Both are correct.

## Testing

- **Add downstream consumer at runtime**: ticker → count, add a `for_each` consumer
  after N cycles; verify it only fires after being added.
- **Add per-instrument subgraph**: source of `(instrument, price)`, aggregation node
  detects new instruments and calls `add_upstream`; verify aggregated totals are correct.
- **recycle delivers first value**: same setup as above but with `recycle: true`; verify
  the value that triggered `add_upstream` is processed through the new subgraph
  (i.e. the new stream fires on the very next engine iteration with the triggering price).
- **Passive upstream**: aggregation node adds a passive upstream; verify it is not
  triggered by it but can still peek its value.
- **Remove node**: add a node, run for N cycles, remove it, run for N more; verify
  stop/teardown are called exactly once and it no longer fires.
- **Remove cleans up caller upstreams**: after `remove_node`, verify the calling node's
  `upstreams` list no longer contains the removed node's index.
- **Layer re-sort**: add an upstream that is deeper than the calling node's current
  layer; verify calling node's layer is updated and execution order remains correct.
- **seen() respected**: add the same node twice; verify setup/start are only called once.
- **ticked() on unregistered node**: call `ticked()` with a node not yet in the graph;
  verify it returns `false` rather than panicking.
