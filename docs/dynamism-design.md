# Runtime graph dynamism for wingfoil-next — design + feasibility spike

Status: **design spike, no engine code changed.** This document establishes the
actual current state of the `wingfoil-next` interpreted engine, the classic
semantics dynamism must reproduce, a concrete proposed API, the engine mechanics
and their hazards, and a phased plan with parity oracles. It closes the
"Dynamic graphs" decision left open in `docs/port-plan.md` (Phase 2 and Phase
4.5) with a recommendation and a ratification checklist for a human to sign off
before implementation.

All claims below are backed by `file:line` references against the branch this
spike is based on (`origin/next`).

---

## 1. Current-state finding — the mutable-frontier dirty-list ALREADY EXISTS

**Finding: commit #471 ("sparse dirty-list dispatch engine") landed. The
wingfoil-next interpreted engine no longer does an O(N) per-cycle sweep as its
production path — it runs a sparse, source-seeded, mutable-frontier dirty-list,
and the old full sweep survives only as an executable reference oracle.**

This directly contradicts `docs/port-plan.md`, which is **stale**: its capability
matrix still lists "Sparse-graph efficiency" as `❌` for "Interpreted (today)"
(port-plan.md:64), Phase 4.5 is written as an unstarted "Gap (must close)"
(port-plan.md:388-399), and the risk register still frames the dirty-list as the
future enabler for dynamism (port-plan.md:553). The code has moved past the plan.

Evidence in `wingfoil-next/src/interp.rs`:

- **Two dispatch strategies exist**, selected by an enum (interp.rs:1498-1505):
  `Dispatch::Sparse` is `#[default]`; `Dispatch::FullSweep` is documented as the
  "original `O(N)`-per-cycle topological sweep, retained as an executable
  reference oracle."
- **`Runner::run` dispatches on it** (interp.rs:1597-1600): `Dispatch::Sparse =>
  self.run_cycles_sparse(...)`, `Dispatch::FullSweep =>
  self.run_cycles_full_sweep(...)`.
- **The sparse loop is a real dirty-list with a mutable frontier**
  (`run_cycles_sparse`, interp.rs:1639-1712):
  - a per-cycle work set held in a `BinaryHeap<Reverse<usize>>` min-heap keyed on
    node **index** (interp.rs:1653);
  - seeded each cycle from a precomputed `seed_nodes` frontier — `always`
    busy-poll ops plus kernel-marked callback-activated ops (interp.rs:1666-1671);
  - propagated forward: a node that ticks marks its **active downstream
    neighbours** dirty and pushes them onto the heap (interp.rs:1690-1697);
  - `node_dirty[]` guards a recombine node against double-enqueue → single-fire
    (interp.rs:1654, 1692-1694);
  - per-cycle reset touches only the nodes that actually fired (`fired` vec),
    keeping work ∝ active nodes, not N (interp.rs:1699-1708).
- **The adjacency + frontier are precomputed once at `build()`**
  (interp.rs:1459-1485): `active_downs[u]` is the reverse of each node's
  `active_ups` (interp.rs:1460-1465), and `seed_nodes` collects the `always` /
  callback-activated nodes (interp.rs:1466-1470).
- **The old sweep is preserved verbatim** as `run_cycles_full_sweep`
  (interp.rs:1720-1749), walking all N nodes each cycle — kept only for
  differential parity testing (`with_dispatch`, interp.rs:1629-1632).

**Why this reshapes the whole analysis.** The port-plan's thesis was that
dynamism needs a dirty-list engine *first*, and that the engine was still an O(N)
sweep. That foundation work is **done**. Dynamism no longer needs a scheduler
rewrite. What it needs is a much narrower thing: a **post-`build()` mutation API**
and the bookkeeping to keep the already-existing sparse structures
(`active_downs`, `seed_nodes`, `node_dirty`/`dirty`/`ticked` vecs, `slots`,
`nodes`) consistent when nodes are appended or deactivated mid-run.

**But there is one deep catch, and it is the crux of this whole design (§4).**
The sparse engine's correctness rests on a specific invariant that classic does
*not* rely on:

> Node **index order is a valid topological order over *all* edges — active and
> passive** — because the fluent API forces a stream to exist (lower index)
> before it can be referenced (interp.rs:1452-1458, 1514-1517).

Classic does **not** need this. Classic dispatches by an explicit per-node
**layer** field (`dirty_nodes_by_layer`, graph.rs:205) and *recomputes layers* on
every dynamic addition (`fix_layers`, graph.rs:1249-1275). That difference is
exactly what makes classic's central dynamism primitive —
`add_upstream(new_node)` **into an existing, lower-indexed caller node** — work,
and it is exactly what next's index-order heap cannot express without help. §4
develops this.

---

## 2. Target semantics — what classic does (the parity oracle)

Classic's runtime dynamism lives behind the `dynamic-graph` cargo feature
(`wingfoil/Cargo.toml:7`, gated throughout `graph.rs`). There are three distinct
mechanisms; only one is true in-place graph mutation.

### 2.1 `graph_node` / `producer` / `mapper` — NOT in-place mutation

`wingfoil/src/nodes/graph_node.rs` spawns an **entire separate `Graph` on a
worker thread** connected to the parent by channels (`producer`,
graph_node.rs:84-94; `GraphMapStream`, graph_node.rs:232-241). The parent graph's
topology never changes — a `ChannelReceiverStream` is the single fixed node that
surfaces the worker's output. This is thread-offloaded static sub-graphs, and
wingfoil-next **already has the equivalent**: `channel()` sources
(interp.rs:320-471) and `produce_async` (port-plan.md:344-348, "landed").

**Implication: `graph_node` needs no dynamism work.** It is already covered by
the channel layer. Do not conflate it with the add/remove feature.

### 2.2 `demux` — fixed topology, dynamic *routing* (NOT add/remove)

`wingfoil/src/nodes/demux.rs` pre-wires a **fixed** fan-out of `size` child
nodes at build time (demux.rs:163-172). At runtime the parent does not add or
remove nodes — it routes by calling `graph_state.mark_dirty(graph_index)` on a
selected pre-existing child (demux.rs:273-274, 424-425), returning `Ok(false)`
itself (it never ticks; it marks a *chosen* downstream dirty directly).

**Implication: `demux` needs a `Ctx`/`Kernel` "mark another node dirty this
cycle" primitive, but NOT node add/remove.** In next this is a modest engine
addition: a way for a node's `cycle` to enqueue an arbitrary node index onto the
current cycle's work set (§4.6). Because demux's children are all higher-layer
pure sinks of the parent, and are wired at build time (so they already have
higher node indices than the parent), next's index-order heap can accept a
"mark dirty at index j > i" during processing of node i — it is exactly what
downstream propagation already does (interp.rs:1690-1697). Demux is therefore the
*cheapest* structural feature to port and is a good early proof.

### 2.3 `dynamic_group` / `add_upstream` / `remove_node` — the real thing

`wingfoil/src/nodes/dynamic_group.rs` is the genuine runtime-mutation client. A
`DynamicGroup` node holds an `add` and a `del` upstream (dynamic_group.rs:156-161)
and in its `cycle`:

- on `add` tick: calls `factory(key)` to build a fresh sub-graph and
  `group.insert(state, key, stream)` → `state.add_upstream(stream.as_node(),
  true, true)` (dynamic_group.rs:111-114, 163-168);
- on `del` tick: `on_remove` fires, then `group.remove(state, key)` →
  `state.remove_node(stream.as_node())` (dynamic_group.rs:119-123, 169-175);
- each cycle: `ticked_iter` visits only the per-key streams that fired this
  engine tick (dynamic_group.rs:126-134, 176-181).

The engine semantics these rest on (`wingfoil/src/graph.rs`):

**Timing — deferred to end of cycle.** `add_upstream` and `remove_node` do not
mutate immediately; they push onto `pending_additions` / `pending_removals`
(graph.rs:369-392). The cycle loop applies them **after** the dirty-list drains
and after `reset()`, removals first, then additions (graph.rs:934-939). So a
mutation requested during cycle *N* takes effect for cycle *N+1*. New nodes never
fire in the cycle that created them.

**Addition mechanics** (`process_pending_additions`, graph.rs:1134-1241):
1. `initialise_node` crawls the new sub-graph, registering only unseen nodes in
   post-order (upstreams before downstreams), appending them at the end of the
   `nodes` vec (graph.rs:1143-1148, 766-837). Existing shared upstreams are
   reused, not duplicated (`seen()`, graph.rs:767).
2. `node_dirty` grows for the new nodes (graph.rs:1150-1153).
3. New nodes' declared upstream edges get their reverse `downstreams` wired
   (graph.rs:1155-1164).
4. The **dynamic caller→node edge** is spliced: the new node becomes an upstream
   of the *existing caller* (a lower index), and the caller becomes a downstream
   of the new node (graph.rs:1170-1186). **This is the topological inversion** —
   an existing lower-indexed node now depends on a higher-indexed new node.
5. `fix_layers(caller_index)` recomputes the caller's layer as
   `max(upstream.layer)+1`, propagates any increase down through its downstreams
   by BFS, and grows `dirty_nodes_by_layer` (graph.rs:1187, 1249-1275). This is
   what re-establishes correct evaluation order after the inversion — **classic
   dispatches by layer, so bumping the caller above the new node fixes it.**
6. If `recycle` (always true from `DynamicGroup::insert`), classic walks up from
   the new leaf to find **attachment points** — new nodes that read a
   pre-existing node, or new sources — and schedules each with
   `add_callback_for_node(ix, t+1ns)` (graph.rs:1188-1216). This is the
   **glitch-free guarantee for dynamic adds**: the new sub-graph fires at `t+1` in
   correct dependency order and observes the shared source's *real current
   value*, not `T::default()` from unrun intermediate nodes.
7. New nodes get their own `setup()` then `start()` called, batched
   (graph.rs:1219-1239).

**Removal mechanics** (`process_pending_removals`, graph.rs:1092-1131): unlink the
node from every upstream's `downstreams` and every downstream's `upstreams`
(graph.rs:1101-1114), run `stop()` then `teardown()` (graph.rs:1115-1127), and set
`active = false` (graph.rs:1128). The `active` flag gates all future cycling
(`cycle_node_inner` returns `Ok(false)` when inactive, graph.rs:993-996).
**Removed slots are never freed** — `node_to_index`, `node_ticked`, `node_dirty`,
`nodes` keep the dead entries (documented memory leak, graph.rs:381-388). Removal
is safe mid-flight because it happens after the cycle drains, never during it.

**Parity oracle tests** (in `graph.rs` `#[cfg(test)]`, all behind `dynamic-graph`):
- `add_upstream_dynamically_fires_only_after_wired` (graph.rs:2102-2139) — added
  at end of cycle 3, ticks on 4/5/6 → 3 ticks. Pins the +1-cycle timing.
- `remove_node_stops_firing_and_calls_lifecycle` (graph.rs:2164-2211) — removal
  stops ticks and calls stop+teardown exactly once, at removal not shutdown.
- `add_upstream_with_recycle_delivers_first_value` (graph.rs:2264) — the recycle
  first-value guarantee.
- `add_upstream_passive_does_not_trigger` (graph.rs:2327) — passive dynamic edge.
- `layer_resort_after_deep_upstream_addition` (graph.rs:2471) — `fix_layers`
  correctness on a deep add.
- `remove_node_that_never_cycled_calls_lifecycle` (graph.rs:2597).
- Integration: `wingfoil/examples/dynamic/{dynamic-group,demux,dynamic-manual}` +
  `examples/dynamic/tests.rs`.

**The static/compiled path rejects dynamism outright**, which we mirror: the
static runner calls `has_pending_dynamic_changes()` and errors — "a compiled
schedule cannot change shape" (graph.rs:1083-1089). See §5.

---

## 3. Proposed API

### 3.1 The core problem the API must solve

`build()` **consumes** the `Builder` (interp.rs:1439, taking `self` by value) and
returns a `Runner` that owns the flattened `nodes`/`slots`/`active_downs`/
`seed_nodes`. The fluent `GraphBuilder` additionally **poisons** itself after
`build()` — a second build or any wire-after-build panics by design
(fluent.rs:39-41, 68-79, 130-141, 265-273; compat review fable-review.md:73,
201-204). So today there is no legal surface to add a node after `build()`, and
the fluent handle-minting path is deliberately dead post-build.

Dynamism needs a *typed* way to (a) mint new nodes+slots against a live `Runner`,
(b) splice edges, and (c) deactivate nodes — all producing `Handle<T>`s that
carry the same `builder_id` guard (interp.rs:67-94) so cross-runner misuse stays
caught.

### 3.2 Recommended surface: an `Extension` scope on the `Runner`

Rather than re-open the consumed `Builder`, expose a **scoped mutation session**
that borrows the `Runner` mutably and re-uses the exact same node-registration
code paths `Builder` already has. The cleanest way to get that code reuse is to
keep the node-construction methods on a shared inner type that both `Builder` and
the extension delegate to; the spike does not require that refactor, only that
the public surface look like this:

```rust
impl Runner {
    /// Open a mutation scope against a *live* graph. Nodes wired here are
    /// appended (highest indices) and their edges spliced; the changes take
    /// effect on the NEXT engine cycle, matching classic's end-of-cycle apply.
    /// Callable only between cycles (see §4.5 for the in-cycle variant).
    pub fn extend(&mut self) -> Extension<'_>;
}

pub struct Extension<'r> { /* borrows Runner: nodes, slots, ticked, kernel ids */ }

impl<'r> Extension<'r> {
    // Same combinator vocabulary as Builder, but each APPENDS to the live graph.
    // Every method returns a Handle stamped with the Runner's builder_id.
    pub fn map<A, B>(&mut self, src: Handle<A>, f: impl Fn(&A)->B + 'static) -> Handle<B>;
    pub fn fold<A, B>(&mut self, src: Handle<A>, init: B, f: ...) -> Handle<B>;
    pub fn ticker(&mut self, period: Duration) -> Handle<()>;
    // ... the single-active-input shape reuses register_op1 unchanged.

    /// Splice a NEW upstream into an EXISTING node. `active` = triggers.
    /// `recycle` schedules the new sub-graph at t+1 in dependency order so it
    /// observes real current values, not defaults (classic add_upstream).
    /// Returns Err if it would make an existing node depend on a higher-indexed
    /// node under the index-order engine — see §4 (the layered engine lifts
    /// this restriction).
    pub fn add_upstream<T>(&mut self, caller: Handle<T>, new: Handle<T>,
                           active: bool, recycle: bool) -> anyhow::Result<()>;

    /// Deactivate a node (and let the caller drop the sub-graph). Runs
    /// stop()+teardown() at end of the current cycle; the node never fires
    /// again. Slots are tombstoned, not freed (classic parity, graph.rs:381).
    pub fn remove(&mut self, node: Handle<impl Sized>);

    /// Commit: recompute active_downs/seed_nodes for the affected region and
    /// grow the per-run scratch (dirty/node_dirty/ticked). Consumes the scope.
    pub fn commit(self);
}
```

**Timing contract.** `commit()` (or drop) stages the changes; the running loop
applies them at the next cycle boundary — reproducing classic's "requested in
cycle N, live in cycle N+1" (graph.rs:934-939). For the **node-internal** case —
a node that wants to grow the graph from inside its own `cycle`, as
`DynamicGroup` does — a narrower `Ctx` hook is needed (§4.5); the `Runner::extend`
surface above serves the **between-runs / driver-thread** case, which is the
simplest first slice.

### 3.3 Interaction with the fluent layer

`GraphBuilder`/`Stream` stay **build-time only** and keep their poisoning
(fluent.rs). Dynamism is a `Runner`/`Handle`-level capability, not a `Stream`
one — consistent with classic, where dynamism is expressed through
`GraphState`/`Node` (`add_upstream` takes `Rc<dyn Node>`, graph.rs:369), never
through the fluent `StreamOperators`. A future `compat::DynamicGroup` facade can
wrap `Extension` the way `dynamic_group_stream` wraps `add_upstream`, but that is
Phase-4-of-this-plan, not the contract.

---

## 4. Engine mechanics

### 4.1 What must grow when a node is appended

The sparse engine keeps several parallel vectors indexed by node position. A
runtime append of `k` nodes must extend all of them consistently:

| Structure | Where | Append action |
|---|---|---|
| `nodes: Vec<NodeRt>` | interp.rs:1519 | push `k` `NodeRt`s (closures capture their own slots) |
| `slots: Vec<Rc<dyn Any>>` | interp.rs:1520 | push `k` slots (already done by the register path) |
| `ticked: Rc<RefCell<Vec<bool>>>` | interp.rs:1521 | push `k` `false` |
| `active_downs: Vec<Vec<usize>>` | interp.rs:1531 | push `k` empty vecs, then add reverse edges (§4.2) |
| `seed_nodes: Vec<usize>` | interp.rs:1534 | push any new `always`/callback-activated node |
| `dirty` (per-run scratch) | interp.rs:1641 | grow to `n+k` `false` |
| `node_dirty` (per-run scratch) | interp.rs:1654 | grow to `n+k` `false` |

The two per-run scratch arrays (`dirty`, `node_dirty`) live **inside**
`run_cycles_sparse` as locals sized once at loop entry (interp.rs:1640-1654). For
in-run growth they must move to fields (or be regrown at the cycle boundary where
a mutation commits). The `Kernel`'s `scheduled: TimeQueue<usize>` already stores
raw indices and needs no resize, but note a hazard: historical `begin_cycle`
writes `dirty[ix] = true` **unchecked** (kernel.rs:201), while realtime wakeups
are bounds-checked (kernel.rs:130-134). A scheduled callback for a not-yet-grown
index would panic — so `dirty` must be grown *before* any schedule targeting a new
node is processed.

### 4.2 Reverse-edge / frontier update is **local to the affected region**

`build()` computes `active_downs` and `seed_nodes` in one O(N+E) pass
(interp.rs:1459-1470). An append does **not** need to redo that pass. For each new
node `i` with active upstreams `U`: push `i` into `active_downs[u]` for each `u ∈
U`, allocate `active_downs[i] = []`, and if node `i` is `always`/callback-activated
push it to `seed_nodes`. That is O(new edges), touching only rows for the new
nodes and their direct upstreams — the "affected region" the port-plan asked for
(port-plan.md:451-455). No existing row changes **as long as new edges point from
lower to higher index** (the append-only case).

### 4.3 The topological-inversion hazard — the central design decision

The sparse loop drains its work heap in **ascending index order** and relies on
"every active downstream has a higher index than its upstream"
(interp.rs:1643-1647). Pure appends preserve this: a new node reads existing
(lower-index) nodes and is read only by even-newer (higher-index) nodes or by
sinks it feeds. **Fine.**

But classic's signature move — `add_upstream(new)` **into an existing caller** —
makes the existing caller (index `c`) depend on a new node (index `m > c`). Under
index-order dispatch the caller at `c` is popped and cycled **before** `m` runs,
so it would read `m`'s stale slot. Classic survives this only because it
dispatches by **layer** and calls `fix_layers` to bump `c`'s layer above `m`
(graph.rs:1187, 1249-1275); index order has no such freedom.

Three ways out (a human must ratify one — see §8):

- **(A) Restrict dynamism to appends (leaf-extension only).** `add_upstream` is
  allowed only when the caller has no *other* downstreams whose order would break
  — in practice, when the "caller" is itself a freshly-appended sink. New
  sub-graphs may read any existing node and feed new sinks, but may not be
  spliced *back into* an existing interior node. This keeps the index-order heap
  and covers a large fraction of real uses (append a monitor/aggregator/output
  onto a live pipeline). It does **not** faithfully reproduce
  `dynamic_group`/`demux`, which splice into a fixed group node. Cheapest;
  smallest blast radius; ships first.
- **(B) Reintroduce an explicit `layer` field and dispatch by `(layer, index)`,
  exactly like classic.** Change the heap key from `Reverse<usize>` (index) to
  `Reverse<(layer, index)>`, store a `layer` per node, and port `fix_layers`
  (graph.rs:1249-1275) verbatim. This makes next's engine a faithful structural
  twin of classic and lifts the inversion restriction entirely, at the cost of a
  per-node field, a layer-recompute on each splice, and re-validating that
  `(layer, index)` ordering reproduces the current byte-identical sparse output
  on the **static** case (it should: for a statically-wired graph, index order is
  already a valid layer order, so the added `layer` key only ever refines ties
  the static tests don't depend on — but this must be pinned by re-running the
  full parity suite under the new key).
- **(C) Hybrid: keep index-order for the static core, maintain layers only for
  dynamically-spliced regions.** More machinery than (B) for no clear gain;
  documented only to be dismissed.

**Recommendation: ship (A) first (Phases 1–2 below), then evaluate (B) before
attempting a faithful `dynamic_group`/`demux` port.** (A) proves the plumbing
(grow-the-vectors, frontier update, +1 timing, removal, recycle) against a real
parity oracle without touching the dispatch key; (B) is the isolated, separately
ratifiable change that unlocks interior splicing. Sequencing them keeps each PR's
blast radius small and each claim independently testable.

### 4.4 Glitch-free single-fire and passive edges across a mutation

Single-fire is preserved for free: the append reuses the same `node_dirty`
double-enqueue guard (interp.rs:1692-1694) — a new recombine node fires once, like
any other. Passive edges are *not* tracked in `active_downs` (they are read, not
triggering — interp.rs:1443-1446, 1528-1530); their correctness comes purely from
index order (a passive reader has a higher index than what it reads,
interp.rs:1452-1458). This is another reason interior splicing (§4.3) is the hard
case: a passive edge from a *new* node into an *existing* reader would silently
break, because the existing reader's index is already fixed below the new node.
Under (A) this cannot arise; under (B) the layer key covers it.

### 4.5 Recycle, self-scheduling sources, and feedback's +1 edge

- **Recycle / first-value delivery.** Classic schedules attachment points at
  `t+1ns` so a new sub-graph observes real values (graph.rs:1188-1216). Next
  already has the mechanism: `Kernel::schedule(index, at)` (kernel.rs:162-164) —
  the same call `feedback_send` uses to fire a *different* node at `time+1`
  (interp.rs:1371-1379). So `Extension::add_upstream(recycle=true)` walks the new
  region's attachment points and calls `kernel.schedule(ix, now+1)` for each —
  a near-verbatim port of graph.rs:1197-1215.
- **Self-scheduling sources** (`SCHEDULES`: ticker, delay, feedback source) that
  are added dynamically must appear in `seed_nodes` (§4.2) *and* have their first
  callback scheduled — for a ticker, its `start` hook does that (interp.rs:588-593),
  so a dynamically-added self-scheduling node must have its `start` invoked at
  commit time, mirroring classic's batched `start` for new nodes
  (graph.rs:1230-1239).
- **Feedback's +1 edge** is unaffected: it is an engine-level scheduled edge
  through the shared `TimeQueue`, keyed on a node index captured at wiring
  (interp.rs:1358-1382). A dynamically-added feedback pair works as long as both
  its source and send nodes are appended together and the source lands in
  `seed_nodes`.
- **In-cycle mutation (the `DynamicGroup` shape).** For a node to grow the graph
  from *inside* its own `cycle`, `Ctx` (op.rs:122-172, today `schedule(at)` +
  clock accessors only) would need a staging hook — e.g. `ctx.request_extend(...)`
  that pushes onto a `pending` list the loop drains at the cycle boundary,
  exactly as classic's `add_upstream` pushes `pending_additions`
  (graph.rs:369-379). This is a deliberate widening of the intentionally-narrow
  `Ctx` (interp.rs:190-197 explains why it is narrow) and should be a **separate,
  explicitly ratified** step — the `Runner::extend` between-cycles surface (§3.2)
  needs no `Ctx` change and is the correct first slice.

### 4.6 Demux — the `mark_dirty` primitive (cheap)

Demux (§2.2) needs only "mark an existing node dirty this cycle" — routing, not
mutation. Since demux children are wired at build time with higher indices than
the parent, a `Ctx::mark_dirty(target_handle)` that pushes `target` onto the
current work heap (guarded by `node_dirty`) is legal under index order for any
`target > self`. Implementation is a few lines against the existing heap
(interp.rs:1690-1697). This makes demux portable **without** the layer question,
and is a strong candidate for an early structural win parallel to (A).

---

## 5. Islands / compiled stay static — by design

The compiled and island paths are, by definition, a **fixed monomorphized
schedule**; their entire value is that the shape is known at compile time
(port-plan.md:454-455, 88-89). Classic already enforces this: the static runner
calls `has_pending_dynamic_changes()` and errors — "a compiled schedule cannot
change shape" (graph.rs:1083-1089). We mirror that exactly.

The port-plan's distinction holds and should be stated in the API docs:

- **A compiled island added *as a unit* at runtime is supported** — the island is
  one opaque node (its interior a straight-line schedule inside a single `cycle`
  closure, `composite`, interp.rs:1393-1432). `Extension` can append a `composite`
  node like any other; the island's interior never changes, only its membership
  in the outer interpreted graph.
- **Mutating an island's *interior* at runtime is NOT supported** — there is no
  frontier inside a `composite` closure to mutate; it is a fused function. A
  caller who needs a dynamic interior must build that region in the interpreted
  engine, not compile it.

This is not a limitation to fix; it is the compiled path's contract.

---

## 6. Value-store coupling — recommendation

The port-plan repeatedly warns that the arena/SoA slot rework and dynamism are
entangled because the slot representation is what every registration closure and
emission captures (port-plan.md:428-436, 462-466, 549). The **critical
already-landed fact**: the interp module froze the slot boundary deliberately —
"Value slots are individual `Rc<RefCell<T>>`s; the arena/SoA store is a
deliberately separate follow-on (the slot boundary is frozen here so the catalog
and adapter ports are not touched twice)" (interp.rs:26-31). Slots are opaque
`Rc<dyn Any>` downcast on access (interp.rs:473-488, 1752-1764); a node's closure
captures its own `Rc<RefCell<T>>` and the engine never indexes `slots` during a
cycle — only `value()`/`slot()` do, by handle.

**Assessment: dynamism does NOT need the arena first, and should NOT wait for it.**

- Appending nodes is *append-only* on `slots` (`new_slot` pushes, interp.rs:484-488)
  — an arena that supported stable handles would append the same way. Nothing about
  add/remove is easier or harder under SoA.
- Removal tombstones (never frees) under **either** representation — classic's
  `Rc<RefCell>` graph already leaks removed slots by design (graph.rs:381-388), and
  an arena would tombstone identically. So dynamism imposes no new constraint the
  arena must satisfy.
- The one real coupling is the reverse: **the arena must preserve handle
  stability across an append** (a `Handle`'s `idx` must keep addressing the same
  value after the store grows). The current `Rc<dyn Any>` vec already gives that
  (push never moves existing `Rc`s' *targets*). An arena/SoA design should
  therefore adopt **grow-only, index-stable handles as a hard requirement** — which
  is exactly the "freeze the slot API boundary" option (b) the port-plan
  recommends (port-plan.md:434-436), and which interp.rs:26-31 says is already the
  posture.

**Recommendation: layer dynamism onto the current slot store now; treat "handles
survive an append, tombstone-don't-free on remove" as the frozen slot-boundary
contract the future arena must honour.** Do not sequence dynamism behind the
arena. If anything, dynamism *ratifies* the frozen boundary by exercising append
and tombstone paths against it — surfacing any hidden assumption before the arena
swap, not after.

---

## 7. Phased implementation plan (with parity oracles)

Each phase is one PR with its classic parity test reproduced on next.

**Phase 0 — enable the oracle.** Turn on classic's `dynamic-graph` feature in the
test matrix and confirm the classic tests (§2.3) are green as the reference.
No next code. Oracle: existing classic tests compile+pass under the feature.

**Phase 1 (first slice) — append a single node onto a live graph.**
`Runner::extend()` → `Extension::map(existing_handle, f)` → `commit()`; the new
map node ticks on the **next** cycle and its value is observable via
`runner.value(handle)`. Restricted to **pure append** (new node reads existing,
is read by nothing yet, or feeds only new sinks) — no interior splice, no `Ctx`
change, no layer field. This exercises: growing `nodes`/`slots`/`ticked`/
`active_downs`/`seed_nodes`/`dirty`/`node_dirty`, the +1-cycle timing, and the
`builder_id` guard on new handles.
*Oracle:* a next port of `add_upstream_dynamically_fires_only_after_wired`
(graph.rs:2102-2139) — add at end of cycle 3, assert 3 ticks on 4/5/6.

**Phase 2 — remove (deactivate) a node + lifecycle + recycle first-value.**
`Extension::remove(handle)` deactivates at the next boundary, runs stop+teardown
once, tombstones the slot; and `add_upstream(recycle=true)` schedules the new
region's attachment points at `t+1` via `Kernel::schedule` (§4.5). Plus the
demux `Ctx::mark_dirty` routing primitive (§4.6), which is independent of the
layer question.
*Oracles:* `remove_node_stops_firing_and_calls_lifecycle` (graph.rs:2164-2211),
`add_upstream_with_recycle_delivers_first_value` (graph.rs:2264),
`remove_node_that_never_cycled_calls_lifecycle` (graph.rs:2597); and `demux_works`
(demux.rs:666-673) for the routing primitive.

**Phase 3 — RATIFY and (if approved) build the layered engine (option B, §4.3).**
Add a `layer` field, switch the heap key to `Reverse<(layer, index)>`, port
`fix_layers`. **Gate:** the entire existing parity suite (catalog, macro,
feedback, channel) must stay byte-identical under the new key —
`with_dispatch`/`Dispatch::FullSweep` (interp.rs:1629, 1720) is the differential
oracle. This unlocks `add_upstream` into an existing interior node.
*Oracle:* `layer_resort_after_deep_upstream_addition` (graph.rs:2471),
`add_upstream_passive_does_not_trigger` (graph.rs:2327).

**Phase 4 — sub-group / `dynamic_group` parity + `Ctx` in-cycle hook.** Widen
`Ctx` with a staged `request_extend` (§4.5) so a node can grow the graph from its
own `cycle`; build a `DynamicGroup`-equivalent (or `compat` facade) on top.
*Oracle:* `wingfoil/examples/dynamic/dynamic-group` end-to-end and
`examples/dynamic/tests.rs`.

**Phase 5 — demux structural parity + overflow.** Full demux/demux_it port on the
`mark_dirty` primitive from Phase 2. *Oracle:* full `demux_works` incl. overflow
and `demux_it`.

---

## 8. Risks & open questions — ratification checklist

A human must decide these before implementation begins:

1. **Dispatch key (the crux, §4.3).** Ship append-only (A) first and defer
   interior splicing, or commit up front to the layered `(layer, index)` engine
   (B)? *Recommendation: A first, B behind an explicit Phase-3 ratification gate.*
   Faithful `dynamic_group`/`demux` parity is **impossible under A** — is
   append-only dynamism acceptable for v1?
2. **Mutation entry point (§3.2, §4.5).** Is the between-cycles `Runner::extend`
   surface sufficient for v1, or is the in-`cycle` `Ctx::request_extend` hook
   (which widens the deliberately-narrow `Ctx`, interp.rs:190-197) required from
   the start because the real clients (`DynamicGroup`) mutate from inside a node?
3. **Timing contract.** Confirm "requested in cycle N, applied at the N/N+1
   boundary, live in N+1" (classic, graph.rs:934-939) is the contract next
   commits to — it is observable and pinned by the oracle tests.
4. **Removal = tombstone, never free (§6).** Accept classic's documented leak
   (graph.rs:381-388) for v1? A compacting/free-list design is strictly more work
   and changes handle semantics (a freed index could be reused) — recommend
   tombstone-only for v1 to keep `Handle` stability trivial.
5. **`dirty` unchecked write hazard (§4.1).** Ratify that the mutation-commit
   point grows `dirty`/`node_dirty` *before* any schedule can target a new index
   (kernel.rs:201 writes unchecked) — a test must cover a dynamically-added
   self-scheduling source.
6. **Fluent/`Stream` stays static (§3.3).** Confirm dynamism is a
   `Runner`/`Handle` capability only and `GraphBuilder` keeps its post-build
   poison (fluent.rs:39-41) — no fluent `.add_child()` surface in v1.
7. **Feature gating.** Mirror classic's `dynamic-graph` cargo feature
   (Cargo.toml:7) so the append/frontier-growth code (and any `Ctx`/heap-key
   change) is opt-in and off the default hot path until proven.
8. **Port-plan is stale (§1).** `docs/port-plan.md` must be updated to record that
   the Phase 4.5 dirty-list **already landed** (#471) and that dynamism is now a
   narrow API-plus-bookkeeping effort, not a scheduler rewrite.

---

## Appendix — key evidence index

| Claim | Location |
|---|---|
| Sparse dirty-list is the default engine | `wingfoil-next/src/interp.rs:1498-1505, 1597-1600` |
| Mutable-frontier sparse loop | `wingfoil-next/src/interp.rs:1639-1712` |
| Frontier/adjacency precomputed at build | `wingfoil-next/src/interp.rs:1459-1485` |
| Old O(N) sweep kept as oracle | `wingfoil-next/src/interp.rs:1720-1749` |
| Index-order topological invariant | `wingfoil-next/src/interp.rs:1452-1458, 1514-1517, 1643-1647` |
| Slot boundary frozen, arena deferred | `wingfoil-next/src/interp.rs:26-31, 473-488` |
| `build()` consumes builder | `wingfoil-next/src/interp.rs:1439` |
| Fluent poisons after build | `wingfoil-next/src/fluent.rs:39-41, 130-141, 265-273` |
| Kernel schedule / begin_cycle unchecked dirty | `wingfoil/src/codegen/kernel.rs:162-164, 201` |
| Classic layer dispatch (`dirty_nodes_by_layer`) | `wingfoil/src/graph.rs:205, 926-940` |
| `add_upstream`/`remove_node` deferred | `wingfoil/src/graph.rs:369-392` |
| Pending applied end-of-cycle (remove, then add) | `wingfoil/src/graph.rs:934-939` |
| `process_pending_additions` incl. edge splice | `wingfoil/src/graph.rs:1134-1241` |
| `fix_layers` recompute+propagate | `wingfoil/src/graph.rs:1249-1275` |
| Recycle attachment-point scheduling | `wingfoil/src/graph.rs:1188-1216` |
| `process_pending_removals` (unlink+lifecycle+inactive) | `wingfoil/src/graph.rs:1092-1131` |
| Removed slots leaked (documented) | `wingfoil/src/graph.rs:381-388` |
| Static runner rejects dynamism | `wingfoil/src/graph.rs:1083-1089` |
| `DynamicGroup` client | `wingfoil/src/nodes/dynamic_group.rs:111-134, 163-181` |
| `demux` fixed-topology `mark_dirty` routing | `wingfoil/src/nodes/demux.rs:273-274, 424-425` |
| `graph_node` = worker-thread sub-graph (not mutation) | `wingfoil/src/nodes/graph_node.rs:84-94, 232-241` |
| `dynamic-graph` cargo feature | `wingfoil/Cargo.toml:7` |
| Port-plan (stale) Phase 4.5 / dynamism | `docs/port-plan.md:388-466, 553` |
