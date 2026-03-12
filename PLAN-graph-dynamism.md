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
It builds `filter_C → process_C` and calls `state.add_upstream(process_C.as_node(),
UpstreamKind::Active)`. The graph wires `process_C` in and registers it as an active
upstream of `aggregation`. From then on, when `process_C` ticks, `aggregation` is marked
dirty automatically.

The aggregation node maintains an internal `Vec<Rc<dyn Stream<ProcessedPrice>>>` to track
and peek at dynamic upstreams. It uses `state.ticked(stream.as_node())` in `cycle()` to
know which ones fired.

## Scope

### In scope (this PR)
- `state.add_upstream(node, kind)` — wire a subgraph into the graph and register it as
  an upstream of the calling node (active or passive).
- `state.add_node(node)` — wire a subgraph into the graph with no upstream relationship
  to the calling node (e.g. new sinks/consumers).
- `state.remove_node(node)` — deregister a node at the end of the current cycle.
- Layer recalculation when a new upstream lands at the same layer or deeper than the
  calling node (see below).

### Out of scope
- Inserting a node between two existing nodes (requires mutating an existing node's
  declared upstreams, which is architecturally unsupported).

## Public API

### New enum (mirrors `Dep<T>` pattern but at the untyped node level)

```rust
pub enum UpstreamKind {
    Active,
    Passive,
}
```

### New methods on `GraphState`

```rust
/// Wire `upstream` (and its upstream subgraph) into the graph and register it
/// as an upstream of the calling node. `kind` controls whether it triggers
/// the calling node on each tick (Active) or is read-only (Passive).
/// Processed at the end of the current cycle.
pub fn add_upstream(&mut self, upstream: Rc<dyn Node>, kind: UpstreamKind)

/// Wire `node` (and its upstream subgraph) into the graph without creating
/// any upstream relationship with the calling node. Useful for new consumers
/// or side-output sinks.
/// Processed at the end of the current cycle.
pub fn add_node(&mut self, node: Rc<dyn Node>)

/// Deregister `node` at the end of the current cycle:
/// unlinks it from all upstream downstream-lists, then calls stop() + teardown().
pub fn remove_node(&mut self, node: Rc<dyn Node>)
```

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
    pending_additions: Vec<(Rc<dyn Node>, Option<(usize, bool)>)>, // NEW
    //   ^ (node_to_add, Some((calling_node_index, is_active)) | None)
    pending_removals: Vec<usize>,  // NEW — node indices
}
```

### Lifecycle at the cycle boundary

After all dirty nodes have been cycled and `reset()` has run, `Graph::cycle()` calls:

```
process_pending_removals()
process_pending_additions()
```

**`process_pending_removals()`**:
1. For each queued index:
   a. Remove node from all upstreams' `downstreams` lists.
   b. Call `stop()` then `teardown()`.
   c. Set `NodeData.active = false`.

**`process_pending_additions()`**:
1. Record `start_index = nodes.len()`.
2. For each queued `(node, caller)`:
   a. `initialise_node(node)` — recurses through subgraph; `seen()` check prevents
      re-wiring already-present nodes.
3. Collect `new_indices`: all indices `>= start_index` (truly new nodes only).
4. For each new index: push `false` to `node_dirty`.
5. For each new index: update its upstreams' `NodeData.downstreams`.
6. Extend `dirty_nodes_by_layer` if new max layer exceeds current vec length.
7. **Batch setup**: call `setup()` on all new nodes in order.
8. **Batch start**: call `start()` on all new nodes in order (after all setup is done).
9. For each queued `(node, Some((caller_index, is_active)))`:
   a. Add `(new_node_index, is_active)` to `nodes[caller_index].upstreams`.
   b. Add `(caller_index, is_active)` to `nodes[new_node_index].downstreams`.
   c. Run `fix_layers(caller_index)` to recalculate layers if needed.

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

### `cycle_node` guard

```rust
fn cycle_node(&mut self, index: usize) -> anyhow::Result<()> {
    if !self.state.nodes[index].active {
        return Ok(());
    }
    // ... existing logic ...
}
```

## Testing

- **Add downstream consumer at runtime**: ticker → count, add a `for_each` consumer
  after N cycles; verify it only fires after being added.
- **Add per-instrument subgraph**: source of `(instrument, price)`, aggregation node
  detects new instruments and calls `add_upstream`; verify aggregated totals are correct.
- **Passive upstream**: aggregation node adds a passive upstream; verify it is not
  triggered by it but can still peek its value.
- **Remove node**: add a node, run for N cycles, remove it, run for N more; verify
  stop/teardown are called and it no longer fires.
- **Layer re-sort**: add an upstream that is deeper than the calling node's current
  layer; verify calling node's layer is updated and execution order remains correct.
- **seen() respected**: add the same node twice; verify setup/start are only called once.
