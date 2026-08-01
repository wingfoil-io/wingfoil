# Porting wingfoil to the Op pattern

Status: **porting in progress** — the Phase 0 contract spikes have landed and
several later phases are underway (see the ✅/🟡 markers throughout the body).
The `wingfoil-next` and `wingfoil-next-macros` crates now live on this branch
(with tests and lints passing) and implement the target pattern: `Op` trait
(pure semantics, engine-owned state), a sparse dirty-list
interpreted engine (Phase 4.5 scheduling landed), a fully monomorphized
`compiled()` expansion, compiled
islands (`nested()`) mountable in interpreted graphs, busy-spin `poll`
sources, and the `graph!` macro deriving all of it from one fluent wiring
function. This document plans the port of the entire classic codebase onto
that pattern.

## Strategy

**Parallel port with a compat facade, not an in-place rewrite.**
`wingfoil-next` becomes the real engine. The classic `wingfoil` API
(`Rc<dyn Stream>`, `NodeOperators`, `#[node]`) survives as a facade over it
until cutover, so:

- nodes/adapters port one at a time, with the classic test suite as a
  permanent parity oracle;
- Rust downstreams on the classic `wingfoil` API see no breakage until the
  facade is deliberately deprecated at cutover. (The **Python** bindings are the
  exception — they are *replaced*, not facaded: `wingfoil-next-python`
  supersedes legacy `wingfoil-python`, a deliberate breaking change — see
  Phase 6.);
- the port can pause indefinitely at any phase boundary with everything
  shipped still correct.

The shared `Kernel` (clock, schedule queue, run bounds, waker channel)
already serves both engines and is the fixed point of the migration.
Branch-1 retrofit codegen (`wingfoil::codegen::{generate, StaticRuntime,
generate_standalone}` + fingerprints + the build-example crate) has been
retired — `compiled()` and islands supersede it with strictly better
guarantees.

## Capability matrix

What each execution path supports, per wingfoil pattern. Legend: ✅ works ·
🟡 partial · 📅 planned · ❌ not supported **by design** (not a missing
feature — the path's value depends on the constraint).

Classic is the reference the next engine converges toward: the interpreted
engine aims to *match* it, while compiled/island add new fast
paths that trade generality for speed (the ❌s are by-design, not gaps).

The two former interpreted columns ("today" vs "+ dirty-list (4.5)") have
collapsed into one: the **sparse dirty-list scheduler has landed** as the
default interpreted dispatch (see Phase 4.5), so what was the 4.5 column *is*
today's interpreted engine.

| Pattern / capability | Classic wingfoil | Interpreted | Compiled | Island |
|---|:--:|:--:|:--:|:--:|
| Static DAG (map/filter/fold/sample/merge/join/…) | ✅ | ✅ | ✅ | ✅ |
| Shared nodes / fan-out | ✅ | ✅ | ✅ | ✅ |
| Split + glitch-free recombine (single-fire) | ✅ | ✅ | ✅ | ✅ |
| Delay & self-scheduling (`SCHEDULES`) | ✅ | ✅ | ✅ | ✅ |
| Feedback / cycles | ✅ | ✅¹ | ❌ | ❌ |
| Busy-poll ingest (`ALWAYS`) | ✅ | ✅ | ❌ | ❌ |
| External / channel / async sources (`THREADED`) | ✅ | ✅ | ❌ | ❌ |
| Bursts (never latest-wins) | ✅ | ✅ | ❌² | ❌² |
| Historical replay | ✅ | ✅ | ✅³ | ✅ |
| Realtime | ✅ | ✅ | 🟡³ | ✅ |
| Fallible ops / error propagation | ✅ | ✅ | ✅ | ✅ |
| Lifecycle start/stop/teardown | ✅ | ✅ | 🟡⁴ | 🟡⁴ |
| Observe arbitrary intermediate streams | ✅ | ✅ | ❌⁵ | ❌⁵ |
| Runtime-valued config (params/captures from caller) | ✅ | ✅ | ❌⁶ | ❌⁶ |
| Mutable per-node state | ✅⁷ | ✅⁷ | ✅⁷ | ✅⁷ |
| Re-run (independent repeated runs) | ✅⁸ | ✅⁸ | ✅⁹ | ✅⁹ |
| Dynamic graph (runtime add/remove) | ✅ | ✅¹⁰ | ❌ | 🟡¹⁰ |
| Sparse-graph efficiency (work ∝ *active* nodes) | ✅¹¹ | ✅¹² | 🟡¹³ | ✅¹⁴ |
| Dense hot-path speed (measured) | 1× | ~1×¹² | 3–4×¹⁵ | interior 3–4×¹⁵ |

¹ Fluent layer only (engine-level `+1` edge); not expressible inside `graph!`.
² No burst *sources* exist in the macro vocabulary; the pattern is about IO
  ingestion, which the compiled path excludes anyway. Lifting this (with
  busy-poll ingest) is deferred post-v1 — see "Deferred / post-v1 work".
³ Compiled runs its own loop with no external wake, so realtime is
  timer-driven only; historical/timer + data-via-consts is full.
⁴ `start` emitted; `stop`/`teardown` emitted once a macro-expressible op
  needs them (none do yet). Classic runs the full setup/start/stop/teardown
  lifecycle.
⁵ Only the declared output tuple is returned — no runner, no peeking
  intermediate nodes; an island exposes only its single output.
⁶ Compiled takes only `(run_mode, run_for)`; closures see consts + passthrough
  locals (compile-time), not values threaded in at the call. Interpreted
  wiring (and classic) capture any runtime local.
⁷ Classic holds state in `#[node]` struct fields; next holds it in `fold`
  accumulators — combinator closures are `Fn`, so a *mutating capture* (which
  would drift between the interpreted and compiled engines) is a compile
  error. Both express arbitrary per-node state, by different idioms.
⁸ Classic is the reference — a fresh `Graph::run` re-initialises via
  `setup`. The interpreted engine now matches it: the per-node `reset` hook
  (Phase 1, landed) restores each node's state + value slot to its wiring-time
  initial value, so `Runner::run` re-runs for the deterministic historical
  subset (tickers/constants + combinators + feedback). Graphs with
  external/poll/channel sources or nested islands stay single-run by
  construction (consumed producer channels/wakers/interiors) — a second `run`
  errors rather than misbehaving.
⁹ `compiled()` is a plain fn — each call is a fresh independent run.
¹⁰ **Landed** behind the `dynamic-graph` cargo feature. The layered
  `(layer, index)` dispatch (always on) makes it possible: a node appended at
  the highest index can be spliced beneath an existing lower-indexed caller,
  `fix_layers` lifting the caller above it. Surfaces: `Runner::run_dynamic` with
  an `Extension` scope (`map`/`fold`/`filter_value`/`add_upstream`/`remove`,
  active/passive + `recycle`), an in-graph `Builder::dynamic_group` (classic's
  `dynamic_group_stream` twin) that stages insert/remove from its own `cycle`,
  and `Builder::demux` (fixed-topology dynamic *routing* on a same-cycle
  mark-dirty primitive — no add/remove). Parity tests in
  `tests/dynamic_graph.rs`; removed slots are tombstoned, not freed (classic
  parity). The compiled/island interior stays fixed by design, but an island
  can be wired dynamically into the interpreted graph. See the Phase 4.5 note.
¹¹ Classic propagates breadth-first through a dirty-list (work ∝ active
  nodes) — though it still carries an `O(N)` per-cycle reset/scan floor the
  deferred 4.5 arena rework can also improve on. Next resets its *tick* state
  sparsely (only the nodes that fired) but shares the kernel's `O(N)` dirty-flag
  clear; it measures as negligible. Its measurable non-active term is depth, not
  `N` — see Phase 4.5, "What the gate does *not* cover".
¹² Sparse dirty-list dispatch (classic's `dirty_nodes_by_layer` model) has
  landed as the default: per-cycle work ∝ active nodes, results byte-identical
  to classic and to the old full sweep (retained as `Dispatch::FullSweep`, an
  executable oracle). The work ∝ active claim is gated deterministically by
  `sparse_work_is_independent_of_graph_size` (`tests/sparse_graph.rs`), not only
  by benchmarks. The arena/SoA value store — the remaining dense-path
  speedup — is a deferred follow-on with the slot boundary frozen (Phase 4.5).
¹³ Straight-line per-node `if cond` checks (cheap, but every node); region
  gating (skip quiet sub-graphs) is the planned compiled counterpart — though
  the `sparse`/`sparse_wide` benchmarks now measure those checks as cheap enough
  that compiled still beats the dirty-list on a ~97%-quiet graph, which lowers
  the expected payoff (Phase 4.5, "Tier ranking on sparse graphs").
¹⁴ A quiet island isn't cycled — islands already give coarse region gating.
¹⁵ Measured on dense chains; standalone LLVM-fuses trivial chains to near-free.
## Phase 0 — design spikes

Four contract questions, each resolved with a spike + parity test before any
mechanical porting. Order matters: fallibility first (widest blast radius).

### 0.1 Fallible cycle + lifecycle hooks  ✅ **landed**

Done: `Op::cycle` returns `anyhow::Result<Tick<Out>>`; `start`/`stop`/
`teardown` are fallible lifecycle hooks (defaults `Ok(())`). The interpreted
`Runner::run` returns `Result<()>`, reporting the first
start/cycle/stop/teardown error with node context (`node 2 (try_map)
cycle: boom …`) and running cleanup regardless. The `graph!` macro threads
`?` through `compiled()`/`nested()` (both now return `Result`). New ops:
`TryMap` (fallible map), `Sink`/`for_each` (fallible sink), `Finally`
(teardown hook). Parity tests in `tests/fallibility.rs` cover
abort-with-context, teardown-runs-on-error, and clean-run teardown.

Original design notes (retained for reference):

```rust
fn cycle(cfg: &mut Self::Cfg, state: &mut Self::State,
         input: Self::In<'_>, ctx: &mut Ctx<'_>) -> anyhow::Result<Tick<Self::Out>>;

fn start(..)    -> anyhow::Result<()> {}   // exists today, becomes fallible
fn stop(..)     -> anyhow::Result<()> {}   // new
fn teardown(..) -> anyhow::Result<()> {}   // new
```

- `Result<Tick<T>>`, **not** a three-variant enum: `Quiet` is control flow
  (hot path), `Err` is failure (cold path, aborts the run). Keeping them
  separate preserves `?`, `.context()`, and the anyhow chain in op bodies.
- For infallible ops the compiled path constructs `Ok(Tick::Value(x))` and
  matches immediately — LLVM folds the discriminant away; no branch
  survives in the binary. Fallible ops pay one predicted branch, same as
  classic.
- Classic parity contract to reproduce: first error wins and is reported
  with graph context; `stop`/`teardown` still run after a cycle error.
  Errors must name the failing node → `Builder` gains debug labels
  (fluent layer sets them from the bound name; the macro already knows it).
- Touches: every `Op` impl, interp adapters (`CycleFn → Result<bool>`),
  `Runner::run → Result<()>`, macro emission (`?` in the dispatch match),
  islands (composite adapter propagates inner errors outward — falls out
  naturally), `cycle_owned_cfg`, all tests.

### 0.2 Feedback  ✅ **landed**

Done: `Builder::feedback::<T>()` / fluent `g.feedback()` return a source
stream (no upstreams — the graph stays acyclic) plus a clonable
`FeedbackSink<T>`. `stream.feedback(&sink)` wires a pass-through send node
that pushes each value onto a shared `TimeQueue` at `time + 1` and schedules
the source node directly on the kernel (`Kernel::schedule(index, at)` — the
engine-level edge the narrow `Ctx` can't express). The source pops due
values on the next cycle. `tests/feedback.rs` reproduces classic
`feedback_active_works` (1, 11, 111, …) plus a self-sustaining loop and sink
cloning. Fluent-only, as planned. Passive feedback (a `bimap` whose feedback
input is read but doesn't trigger) waits on the passive-input node in
Phase 2 — noted in the test.

Original design notes (retained for reference):

```rust
let (fb_out, fb_sink) = g.feedback::<T>();       // source usable immediately
...
downstream.feed(&fb_sink);                        // close the loop later
```

Sink pushes `(value, time)` into a shared `TimeQueue` (dedup preserved — see
CLAUDE.md: dedup is a feature) and schedules the source node via the kernel,
reproducing classic active/passive feedback timing. V1 restriction: fluent
layer only — not expressible inside `graph!`/islands (a cycle in the island
DAG breaks straight-line emission). Oracle: classic `feedback_works`,
`feedback_active_works`, `feedback_passive_works`, `feedback_sink_clone_works`.

### 0.3 Bursts & channel messages  ✅ **burst pattern, both modes, landed**

Decision (corrected) and implemented (Phase 3): **the burst pattern
throughout — never latest-wins, never a dropped value.** A source emits
`Stream<Burst<T>>` (`wingfoil_next::Burst<T>`), where a burst is every
value occurring at one instant, grouped and delivered atomically in a single
cycle. Same-time values ride *one* burst — they are not coalesced (the
latest-wins bug of my first cut) and not split across the clock by
monotonic bump (the earlier fallback, also wrong). This matches classic
`Burst<T>` / `HistoricalValue(ValueAt<Burst<T>>)`.

Channel sources (`GraphBuilder::channel`) run in **both** modes:
- **Realtime**: waker-driven; a cycle drains all arrived values into one
  burst.
- **Historical**: the producer sends timestamped values
  ([`ChannelSender::send_at`]) then closes; the receiver groups same-time
  values into bursts at `start` and schedules delivery on the graph clock,
  so a wall-clock-arriving async feed replays **deterministically** at its
  timestamps — the classic `produce_async` model. `external` likewise emits
  bursts (realtime-only, no timestamps). `Message::Error` aborts the run via
  the Phase 0.1 fallible cycle. `tests/channel.rs` covers all of it (lossless
  cross-thread delivery, deterministic historical replay, same-time-one-burst,
  error abort, envelope equality). Cross-process serde framing returns with
  the zmq/kafka adapters.

Original design notes (retained for reference):

Classic's channel envelope (`HistoricalValue` bursts, `Checkpoint`,
`EndOfStream`, error variants) vs next's one-value-per-cycle. Decision to
validate: **keep the envelope as-is**; endpoints become ops
(`External`/`Poll` + waker for realtime; a scheduling replay source for
historical). Same-time burst members collapse per the kernel's monotonic
time bump — assert against classic's
`same_time_burst_does_not_break_monotonic_engine_time` and the async_io
burst tests. If parity fails, fall back to a burst payload
(`Tick<Burst<T>>`-style) — decide here, not later.

### 0.4 Re-run / runner lifecycle  ✅ **decided (single-run v1)**

Investigated: a second `Runner::run` *continues* accumulator state (a
counter goes 3 → 6) but each call builds a fresh `Kernel` from t=0, so a
self-scheduling source carries stale scheduling state — a ticker re-runs
with polluted timing (fires at 0, 400, 500 instead of 0, 100, 200).
Accumulators-continue + clocks-restart is not a coherent contract.

**Decision for v1: a `Runner` is single-run** (external/poll already assert
this; timer graphs get the same expectation, documented). Well-defined
re-run — classic's setup-per-run *reset* semantics — needs a per-node
`reset`/`setup` hook (same shape as the `stop`/`teardown` plumbing from
0.1) that restores each op's state to its wiring-time initial value,
including re-seeding schedules.

**The compat facade is the use case, so the reset hook is Phase-1 contract
work — not deferred.** Classic streams re-run, and the Phase 6 facade is
exactly what surfaces that: `compat::Signal` already breaks on a second
`run()` (it silently runs a 0-node graph then panics out-of-bounds on
`peek_value`), and wingfoil-python's pytest suite — the facade gate — depends
on re-run working. Rather than discovering this at the facade, the per-node
`reset`/`setup` hook lands in **Phase 1** alongside the other contract
plumbing (it slots into the existing lifecycle machinery). This closes the
last Phase-0 spike by decision.

**Update — delivered in Phase 1.** The `reset` hook has since landed (see the
Phase 1 bullet): the interpreted `Runner` re-runs the deterministic historical
subset, restoring per-node state + slots to their wiring-time values, and
`compat::Signal::run` is re-runnable. Single-run graphs (external/poll/channel/
island) error on a second `run` rather than misbehaving.

**Single-run I/O is classic parity (verified 2026).** The single-run restriction
for I/O sources is *not* a deviation from classic: classic is single-run for
them too. Classic builds a fresh `Graph` over the shared node tree each `.run()`,
and its `AsyncProducerStream::setup` (`wingfoil/src/nodes/async_io.rs:214`) takes
its `func`/sender with `.take().ok_or_else(|| "func is already taken")?` — so a
second run **errors**; `ChannelReceiverStream::setup` (`nodes/channel.rs`) drains
its receiver and consumes its notifier, so a second run produces nothing. next's
explicit single-run error is therefore parity (and clearer). See deviation
register A2 — the earlier "classic re-runs I/O sources" claim was incorrect.

**Gate 0:** all four spikes land with classic-parity tests green.

## Phase 1 — contract completion

- Fold spike results into `op.rs` + all three engines + macro.
- Variadic gaps: `Join3` (trimap) ✅, n-ary merge ✅, `try_map`/`try_bimap`/
  `try_trimap` ✅ (trivial once cycle is fallible — the closure returns
  `Result`, the op `?`s it). **n-ary merge** landed as `StreamOps::merge_all`
  (`fluent.rs`) — sugar that unrolls to a left-associated chain of 2-ary
  `merge`s. 2-ary merge's earliest-wins tie-break is associative, so the chain
  and a single variadic node fire identically; the sugar-over-primitive
  approach matches `fan`/`map_n`, and a dedicated variadic op would add engine
  complexity for zero observable difference (`tests/rerun.rs::
  merge_all_matches_chained_merge`).
- **Multi-output islands — re-deferred (written rationale).** An `Op`/island
  produces a single `Out`; classic multi-output nodes (`demux`,
  `dynamic_group`) would need projection nodes that fan a tuple output into N
  slots. Re-deferred for v1 because: (a) nothing in the catalog *ported so far*
  needs it — every Phase 2 op landed is single-output, and the two classic
  multi-output nodes are in the "Structural / deferred" group that also awaits
  the dynamic-graph decision (Phase 4.5); (b) an island already exposes its one
  output cleanly, and a caller wanting K outputs can mount K islands over the
  same inputs (wasteful only if the shared interior is expensive — none is
  today); (c) the honest projection design is coupled to the Phase 4.5 arena
  slot representation (a projection writes several slots from one cycle), so
  building it against today's `Rc<RefCell<T>>` slots risks the exact
  touch-it-twice rework the Phase 4.5 coupling note warns against. Decision:
  revisit alongside `demux`/`dynamic_group` in the Phase 4.5 dynamic-graph
  pass, not as isolated Phase 1 work.
- Debug labels on nodes (needed by 0.1 error reports; also unlocks GML
  export in Phase 5).
- **Per-node `reset`/`setup` hook** ✅ **landed** (from spike 0.4) — the
  interpreted engine now restores each node's engine-owned state and value slot
  to its wiring-time initial value before a re-run, giving well-defined
  repeated runs. `register_op1`/`register_op2` take a *state factory*
  (`Fn() -> S`, not a value) so the engine can re-seed; the `#[op]` macro emits
  `|| Default::default()`. `NodeRt` carries a `reset` closure (same plumbing
  shape as `stop`/`teardown`), set by every registration. `Runner::run`
  auto-resets on a second call (and `Runner::reset()` is public); the kernel is
  rebuilt from `t=0` each run and each node's `start` re-seeds its schedules, so
  reset only touches per-node state. Graphs with `external`/`poll`/`channel`
  sources or `composite` islands are single-run by construction (their producer
  channels, wakers, and island interiors are consumed by the first run) and a
  second `run` errors clearly. This unblocks the Phase 6 compat facade:
  `compat::Signal::run` is now re-runnable, the wingfoil-python re-run gate.
  Covered by `tests/rerun.rs` (re-run == fresh graph; fold restarts not
  continues; buffering/sampling ops reset; feedback re-runs; explicit reset;
  single-run guard) and the rewritten `tests/compat.rs::
  second_run_matches_the_first`.
- **`Tick::Silent` (update-value-without-ticking) contract decision** — the
  `Tick` contract today cannot express "store a new value but don't tick,"
  which classic relies on (e.g. `Delay`'s first-value seeding, so passive
  readers never see `T::default()` before the delay elapses). This is a
  Phase-1 contract-shape decision — add a `Tick::Silent(T)` variant (or
  equivalent) or document the deviation — **not** a delay-porting detail:
  deciding it late risks retrofitting every emitter, the exact mistake the
  plan avoided by doing fallibility first. Blocks the `delay` port in Phase 2.

## Phase 2 — the node catalog

Recipe per node, in this order, no exceptions:

1. identify `Cfg` / `State` / `In<'a>` / `Out` / `ACTIVATION`;
2. move the classic `cycle` body verbatim into the op (same logic, inputs
   passed in instead of read from upstream `Rc`s);
3. wire it up (see **Adding an op** below): `#[op(build = name)]` on the impl
   generates the interpreted `Builder` method from the op's `In` shape *and*
   the `graph!`/compiled forwarders the emission dispatches through; add the
   fluent method (the one piece still hand-written);
4. port the classic node's unit tests as parity tests (values **and** tick
   times).

### Adding an op — current tooling

Two mechanisms single-source most of the boilerplate; the residual per-op cost
is small and explained by two hard constraints on proc macros:

- **A proc macro sees tokens, not resolved types** — so `graph!` cannot
  introspect an `Op` impl to learn its arity/cfg/input shape. Any per-op
  knowledge the macro needs must be written in the macro crate.
- **A trait cannot be extended from scattered sites** — so `#[op]` cannot add a
  method to `StreamOps`; the fluent method stays hand-written (a 3-line
  one-liner), or would have to be inherent-on-`Stream`.

What's automated:

- **Interpreted engine** — `#[op(build = name)]` on `impl Op for X` generates
  `Builder::name` from the op's declared shape: one `Handle` parameter per edge
  of `In<'a>`, in order, then the `Cfg` (omitted when it is `()`). This covers
  **every** shape the macro parses — sources (`In = ()`), single- and
  multi-input ops, edges read with their tick flag (`(&'a T, bool)` — `delay`,
  `merge`), `passive = [..]` non-activating edges, `start`/`stop`/`teardown`
  lifecycle hooks, and `init_arg` seeded accumulators — so no op keeps a
  hand-written `Builder` method for want of tooling. Node labels come from
  `type_name::<X>()` (shortened), not hand-written strings. `no_builder` is
  left for the case where the interpreted *signature* deliberately differs
  from the shape: `with_time` (seeds its value slot from the input's current
  value, so it never requires `Out: Default`) is the catalog's only one. The
  `bimap`/`trimap` family are *additional* hand-written methods over the
  `Join`/`Join3` ops — their active/passive split is a runtime argument rather
  than the compile-time `passive` mask — alongside the generated `join` /
  `join_passive` / `join3`. The generated body is the same
  `next_node_index` → `slot`/`new_slot` → `push_node` → `set_*` sequence a
  hand-written builder contains, against a `pub(crate)` seam on `Builder`; the
  public `register_op1`…`register_op4` primitives remain the wiring path for
  **out-of-crate** ops, which write their forwarders by hand (`#[op]`'s output
  names `crate::…`, so it is in-crate tooling — see `tests/custom_op.rs`).
- **Compiled / `graph!`** — one `OpKind::info()` row per op (an `OpInfo`:
  op type, dispatch flags, and the `Inputs`/`CfgInit`/`StateInit` shapes) drives
  every emitter. Named fields make a half-filled row a compile error.

So the places to touch when adding an op — **the compiled path is
zero-touch; there is no macro table**:

| Op shape | Interpreted | `graph!`/compiled |
|---|---|---|
| Single-input | `ops.rs` (`impl` + attr) + fluent method | nothing — `#[op]`'s forwarders cover it |
| Multi-input, values-only, all-active (the `join` shape) | same — `ops.rs` (`impl` + attr) + fluent method | nothing — `&stream` args classify as edges |
| Source (`In = ()`), lifecycle hooks, tick-flag edges | same — `ops.rs` (`impl` + attr) + fluent method | nothing |
| Passive edges (`passive = [..]`) / seeded accumulators (`init_arg`) | same — `ops.rs` (`impl` + attr, with the flag) + fluent method | nothing — attribute flags on `#[op]` |
| Interpreted signature ≠ the op's shape (`with_time`) | `no_builder` + a hand-written `Builder` method + fluent method | nothing |

Constraint #1 still holds (a proc macro sees tokens, not types), but it is
routed around rather than paid per-op: every method call is emitted through
`#[op]`-generated forwarder functions by naming convention, rustc's
inference resolves the op type the macro never names, and per-op facts
(`ACTIVATION`, passive-edge masks) are re-emitted as consts the emission
folds on. Delay's engine-level special cases became `Tick::Silent` in the
`Op` contract. Measured at parity with the deleted table emission and
covered by `wingfoil-next/tests/custom_op.rs`; full analysis in
`macro-extensibility-decision.md`. The fluent method remains
hand-written (constraint #2, unchanged).


**Completeness test ✅ (Phase 1) — realized as a compile-guard.** The original
plan (a `supported_ops!()` list diffed against the fluent surface) assumed the
`graph!` **op table / parse-match** that the forwarder refactor since *deleted*
(see `macro-extensibility-decision.md`): `graph!` now dispatches through
naming-convention forwarders (`__wf_op_<name>_*`), so there is no central op
list to diff and an unknown op simply fails to resolve a forwarder. The guard
is instead realized at **compile time** in `tests/op_completeness.rs`: a
combinator *used inside* a `graph!` block only compiles if it has **both** a
fluent method (the wiring fn is fluent code) **and** a forwarder (`#[op]`), so
exercising every dual-mode combinator there is exactly the two-sided
one-sided-registration guard, and each block additionally asserts
`interpreted() == compiled()`. The by-design fluent-only surface (feedback, IO
sources `external`/`channel`/`poll`, the `for_each` sink) is documented in that
file as the explicit allowlist. Building it also **surfaced three ops that are
currently interpreted-only for want of a forwarder** — `join_passive` /
`try_join_passive` (passive-edge joins), `delay_with_reset`, and `with_time`
(all hand-written builder methods, not `#[op]`); these are candidate follow-ups
(give them `#[op]` or an equivalent forwarder), not by-design gaps.

Inventory (classic `nodes/` → target), grouped by effort:

| Group | Nodes | Notes |
|---|---|---|
| Done in prototype | map, filter, fold, constant, sample, merge (2-ary), delay, tick(er), producer(→poll), consumer(→for_each), try_map, finally, feedback | parity-tested |
| Trivial state/closure | ✅ distinct, difference, limit, map_filter, throttle, inspect, logged, window, buffer, with_time, ticked_at/-elapsed, not, print, timed (`tests/catalog.rs`, `tests/catalog_ops.rs`); ✅ split, combine, collapse (Burst/tuple structural, `tests/catalog_flow.rs`) | recipe proven; `window`/`buffer` use `Ctx::is_last_cycle`; `combine` builds the burst locally (no shared-cell port). `print` prints per-tick (deviation D8, dropping classic's teardown buffer); `logged` is fluent-only (deviation D9) — `&str` label vs `String` cfg |
| Scheduling | ✅ throttle, delay_with_reset, node_flow (node-level delay/filter/limit/throttle, run over the unit-stream path, `tests/catalog_flow.rs`) | `SCHEDULES`/time-gated; pattern proven by delay + throttle |
| Multi-input | ✅ bimap (active/passive) + join, trimap + join3, try_* variants (`tests/catalog_ops.rs`) | passive `bimap` unlocked passive feedback; `trimap` is the 3-ary combine |
| Engine-touching | always (→`ALWAYS`, done), ✅ never (`tests/catalog_flow.rs`), finally (needs teardown, done), callback stream, iterator_stream (replay source; needs 0.3), receiver, channel nodes (→Phase 3), async_io (→Phase 3) | |
| Structural / deferred | demux ✅, dynamic_group ✅, ✅ graph_node (→`spawn`/`spawn_map`, `tests/spawn.rs`) | multi-output + dynamic-graph notes below |

**`graph_node` (thread-offload) ✅ ported as `spawn` / `spawn_map`.** classic
`graph_node` is two combinators — `producer()` (a source sub-graph on a worker
thread) and `.mapper()` (map an input stream through a worker sub-graph). Their
next twins are `SourceOps::spawn` and `StreamOps::spawn_map` (`fluent.rs`), riding
the channel layer: the worker builds and runs its own graph at run start (under
the driving run's inherited mode + bound) and exchanges timestamped values over
the channel. Both run in **both** modes. Historical mode is deterministic and
lock-step by graph time — which required replacing the channel's historical
*block-collect* with an **incremental, timestamp-gated read** (classic's
"block-while-behind / non-block-once-caught-up" loop, `interp.rs::pump_historical`)
so a worker that depends on the driving graph's output no longer deadlocks. That
incremental read also gives every channel bounded (one-ahead) memory. Parity
tests in `tests/spawn.rs`; the classic `graph_node_works` oracle. *Lock-step
caveat (matches classic):* the sub-graph is expected to emit a result per input
instant; bound historical `spawn_map` runs by duration, not a raw cycle count
(the lock-step reader spends one no-op poll cycle between instants — a next
monotonic-clock artifact with no effect on values/times).

**Dynamic graphs** (`dynamic_group`, the dynamic examples): distinct from
`graph_node` above — this is runtime graph *mutation* (add/remove nodes mid-run),
not thread-offload. islands already cover *static* subgraphs composed procedurally
(including in loops). Runtime mutation has since landed as a separate feature
(behind `dynamic-graph`): `Runner::run_dynamic` + an `Extension` scope
(append / active-passive splice / remove / recycle), `Builder::dynamic_group`,
and `Builder::demux` on the interpreted engine.

**Engine coverage note:** `never` and `combine` land as interpreted-engine
(fluent) ports — like `feedback` — since a source that never ticks and an
n-ary fan-in have no `Op` witness for `#[op]` to hang forwarders on (`combine`'s
arity is a runtime slice, which the fixed-arity tuple `In` cannot express).
`delay_with_reset` since reached all three engines through its
`DelayWithResetFwd` witness op, which restates its two-tick-flag `In` in the
uniform one-`(value, tick)`-pair-per-edge form `#[op]` parses.
`split`/`collapse` are pure sugar over `map`/`map_filter`, so they reach every
engine for free. Extending `combine` to `graph!`/compiled is a follow-up (as it
is for `feedback`).

**Gate 2:** every classic node test has a next twin producing identical
values and tick times.

## Phase 3 — channel layer, threading, async

- ✅ Channel endpoints on ops: `channel::Message` envelope +
  `GraphBuilder::channel()` receiver source emitting `Stream<Burst<T>>`,
  running in **both** modes (realtime waker-driven, historical deterministic
  replay of timestamped sends), + `ChannelSender` (send / send_at /
  send_error / checkpoint / close), with error propagation through the
  fallible cycle. `external` also emits bursts. `tests/channel.rs`.
- ✅ `produce_async` ergonomic (async closure → timestamped burst stream)
  over the channel, gated behind the `async` feature (tokio + futures):
  `async_source::produce_async(&g, handle, params, |p| async {...})` matching
  classic. `tests/produce_async.rs` (deterministic historical replay,
  same-time-one-burst, mid-stream error abort) + `produce_async_feed`
  example.
- ✅ `consume_async` ergonomic — the **sink** counterpart of `produce_async`,
  completing the source/sink async symmetry (the classic `consume_async`),
  gated behind `async`: `async_source::consume_async(handle, buffer_size, |v|
  async {...})` returns a closure to plug into `for_each`. A single background
  consumer task drains each burst so **write order is preserved**; a bounded
  channel (`buffer_size`) applies back-pressure (the sink closure blocks the
  graph thread on a full channel, both run modes); a write error propagates
  into the graph over an error channel and aborts the run on the next cycle;
  teardown flushes all queued writes. `tests/consume_async.rs` (order,
  bounded back-pressure, error-abort). Known limitation: a write error from the
  **last** cycle of a bounded run has no later cycle to surface it (the teardown
  flush cannot turn an `Ok` run into `Err`), so a sink that must abort
  deterministically on its final write cannot migrate to it yet (see etcd).
- ✅ Classic `threading`/`async` examples re-implemented on next. `threading`
  (`examples/threading/`) offloads a producer sub-graph to a worker thread that
  feeds the main graph over the channel layer — the primitive under classic
  `producer()`/`mapper()` (the `graph_node` node), which now **also** have direct
  fluent twins, `SourceOps::spawn` / `StreamOps::spawn_map` (see the Phase 2
  `graph_node` entry) — and runs in both modes (realtime bursts, deterministic
  historical replay). `async` (`examples/async/`, gated on the
  `async` feature) drives the graph from a `produce_async` producer with the
  graph as consumer. Bounded-buffer back-pressure (`produce_async`'s optional
  `buffer_size`, applied in **both** run modes — register B5) and
  wiring-time `RunParams` (validated against the real run in historical mode)
  landed with the `produce_async` ergonomic above.

## Phase 4 — adapters, easiest-first

Order chosen by (pure → request-shaped → streaming → build-painful):

1. **statistics** — pure computation, the largest single chunk, huge test
   suite, zero IO. Best stress test of engine-owned state; do it first.
   ✅ *done*: the statistics families are ported with parity
   tests — exponential (`Ewma`, PerTick + clock-driven HalfLife,
   `tests/statistics.rs`); count-windowed rolling
   (`RollingSum`/`Mean`/`Min`/`Max`/`Var`/`Std`/`Median` — monotonic-deque and
   incremental-moment variants, `tests/statistics_rolling.rs`); cumulative
   / unbounded (`CumulativeSum`/`Mean`/`Min`/`Max`/`Var`/`Std`/`Median` —
   Welford online moments for mean/var/std, `tests/statistics_cumulative.rs`);
   and **time-windowed** rolling (`TimeWindowedSum`/`Mean`/`Min`/`Max`/`Var`/
   `Std`/`Median` over a bounded `Window::Time`, count-weighted — incremental
   sum/moments, monotonic-deque min/max, recompute median,
   `tests/statistics_time_windowed.rs`). The time-*weighted* moment path
   (`Weighting::Time`, mean/var/std over all three windows —
   `{Cumulative,Rolling,TimeWindowed}{Mean,Var,Std}TimeWeighted`, West's
   incremental weighted moments with an exact `remove` inverse for the sliding
   windows, `tests/statistics_time_weighted.rs`) is ported. The final classic
   statistics capability — the time-*weighted* **median**
   (`median(_, Weighting::Time)` → the classic `WindowStream::weighted_median`;
   `{Cumulative,Rolling,TimeWindowed}MedianTimeWeighted`, a recompute-per-tick
   weighted median over the retained `(value, time)` window rather than a moment
   accumulator, `tests/statistics_time_weighted_median.rs`) is now ported too, so
   the statistics adapter is complete.
2. **cache**, **common** (WindowFilter) — small, pure.
   ✅ *done*: **common** ports the always-compiled `TimeWindow`/`WindowFilter`
   out-of-window row filter (`adapters::common`, `tests/common_adapter.rs`);
   **cache** ports the file-backed, query-keyed, LRU-evicting result cache
   (`CacheKey`/`CacheConfig`/`FileCache`) behind the `cache` feature
   (`adapters::cache`, `tests/cache_adapter.rs`, classic unit tests ported
   verbatim). **Deviations** (none behavioural): (a) the classic time-slicing
   helpers (`compute_time_slices`/`compute_validated_time_slices`) land in
   `common` alongside the time-sliced readers — **ported with `postgres`**
   (Phase 4 item 4) and feature-gated on `any(postgres, kdb)` — the kdb port
   (Phase 4 item 5) widened the gate to reuse the same slicer; the always-compiled
   `WindowFilter`/`TimeWindow` surface is here from the start; (b) `FileCache`'s
   log messages drop the classic "KDB " prefix (the cache is not kdb-specific in
   next).
3. **csv** — replay source + sink; exercises 0.3 historical bursts.
   ✅ *done*. The `csv` and `lines` adapters share two fluent primitives so the
   source/sink boilerplate lives in one place: `GraphBuilder::replay_results`
   (queue a finite `Result<(value, time)>` sequence onto a `channel` source and
   close it — the decode-error-then-stop shape `csv_read` needs) and
   `StreamOps::for_each_mut` (the `&mut`-writer sink, wrapping the owned resource
   in a `RefCell` once instead of in every sink). **Deviation (B4)**: next's
   `csv_read` reads and deserializes the whole file up front (it queues every row
   onto the channel source before the run), whereas classic's `TryIteratorStream`
   streams rows lazily; behaviour is identical for finite files, but next holds
   the full row set in memory and surfaces a decode error at the start of replay
   rather than mid-stream. `csv` also gains the single-value `CsvSinkOps for
   Stream<T>` convenience (auto-wrapping into a one-element burst, matching
   `etcd`); `lines` deliberately keeps its sink burst-only, because `Burst<T>`
   *is* `Display` — a `Stream<Burst<T>>` would be indistinguishable from a
   single-value `Stream<T: Display>`, so the same convenience is ambiguous there.
4. **redis, postgres, etcd** — request/response shaped; fallible cycle +
   lifecycle hooks.
   ✅ **etcd** *(done)*: snapshot→watch source (`etcd_sub`) on `produce_async`
   and a key-value PUT sink (`EtcdSinkOps::etcd_pub`) with leases (background
   keepalive + revoke-on-teardown via a `Drop` guard) and the `force`
   conditional write, behind the `etcd` feature. Parity port of the classic
   adapter's tests as `tests/etcd_integration.rs` (testcontainers, gated on
   `etcd-integration-test`) plus no-service tests in `tests/etcd_adapter.rs`.
   **Deviations:** all classic capabilities preserved; after the systemic
   defer-to-start migrations the snapshot→watch source and the PUT sink both
   establish their I/O at run start, not wiring — the graph owns the tokio
   runtime (no `&Handle`; register A5), `etcd_sub` takes only a `RunMode` and
   **rejects `RunMode::HistoricalFrom` at wiring** (a live, unbounded watch with
   no historical timeline to replay — register B2, ratified), and `etcd_pub` now
   writes off the graph thread via `consume_async` — its `flush` teardown
   surfaces the final-cycle `force: false` conditional-write abort, so the old
   per-write `Handle::block_on` is gone (register A1/A4/B1). The canonical
   deviation list is the adapter's `# Deviations from classic` module-doc block
   plus [`deviation-register.md`](./deviation-register.md).
   ✅ **redis** *(done)*: Pub/Sub (`redis_sub` source + `RedisSinkOps::redis_pub`
   sink) and Streams (`redis_stream_read` snapshot→tail source +
   `RedisStreamSinkOps::redis_stream_write` sink), behind the `redis` feature.
   Both sources ride `produce_async`; both sinks ride the shared **`consume_async`**
   primitive (redis has no per-write conditional to abort synchronously, unlike
   `etcd_pub`, so the off-thread sink fits — writes land in order and flush at
   teardown). Parity port of the classic adapter's tests as
   `tests/redis_integration.rs` (testcontainers, gated on `redis-integration-test`)
   plus no-service tests in `tests/redis_adapter.rs`; classic example ported to
   `examples/redis_adapter.rs`. **Deviations:** all classic capabilities
   preserved; after the defer-to-start migrations both Pub/Sub and Streams
   sources ride `produce_async` and both sinks ride `consume_async`, all
   establishing their I/O at run start rather than wiring — the graph owns the
   tokio runtime (no `&Handle`; register A5), the sinks connect lazily on their
   first write (register A1/A4), and both sources take a `RunMode`, **rejecting
   `RunMode::HistoricalFrom` at wiring** (live, unbounded streams — Pub/Sub has
   no backlog, the stream tail blocks forever — with no historical timeline to
   replay; register B2, ratified). Burst-model note: `redis_stream_read`'s
   snapshot rides one atomic burst (one shared timestamp, as etcd). The canonical
   deviation list is the adapter's `# Deviations from classic` module-doc block
   plus [`deviation-register.md`](./deviation-register.md).
   ✅ **postgres** *(done)*: a time-partitioned historical replay source
   (`postgres_read` — one query per midnight-aligned time slice, clamped by
   `WindowFilter`, fed to `replay_results`), a realtime `LISTEN`/`NOTIFY`
   live-tail source (`postgres_sub`), and a streaming insert sink
   (`PostgresSinkOps::postgres_write`, per-burst pipelined via `consume_async`),
   behind the `postgres` feature on the async `tokio-postgres` client. **First
   time-partitioned adapter to port**, so it lands the shared time slicer
   (`compute_time_slices`/`compute_validated_time_slices`) in `adapters::common`
   (feature-gated `postgres` now; kdb adds its feature at item 5) alongside the
   already-present `WindowFilter`/`TimeWindow`. Parity port of the classic
   adapter's tests as `tests/postgres_integration.rs` (testcontainers, gated on
   `postgres-integration-test`) plus no-service tests in
   `tests/postgres_adapter.rs`; classic example ported to
   `examples/postgres_adapter/`. **Password redaction** (classic PR #433) is
   reproduced: `PostgresConnection::redacted()` masks the DSN `password=…` token
   at every `connect()` error site. **Deviations:** all classic capabilities
   preserved; after the defer-to-start migrations the graph owns the tokio
   runtime (no `&Handle`; register A5), the `postgres_write` sink connects lazily
   on its first write inside `consume_async` (register A1/A4), and `postgres_read`
   now defers its connect + slice queries to the run via `produce_async` (register
   B5 resolved) — the window is still validated + sliced at **wiring** (a pure,
   fail-fast check), but no I/O happens there, so a connection/query error aborts
   the run rather than graph construction. The `LISTEN`/`NOTIFY` live tail
   **rejects `RunMode::HistoricalFrom` at wiring** (classic parity — classic
   `postgres_sub` already required realtime; register B2). **Unified source
   landed (register B2):** `postgres_source(PostgresSourceConfig)` dispatches on
   `params.run_mode` at wiring — historical → `postgres_read`, realtime →
   `postgres_sub` — with the two primitives kept public underneath. The canonical
   deviation list is the adapter's `# Deviations from classic` module-doc block
   plus [`deviation-register.md`](./deviation-register.md).
5. **zmq, kafka, kdb** — streaming; `poll`/`external` + lifecycle.
   ✅ **zmq** *(done)*: real-time ØMQ pub/sub — a `zmq_sub` source (a background
   OS thread polling the `SUB` socket + monitor, feeding the `channel` layer,
   returning a `(data, status)` pair) and a `ZeroMqPub::zmq_pub` /
   `zmq_pub_on` sink (a `register_op1` op that binds the `PUB` socket at graph
   `start()`, buffers around the ZMQ slow-joiner, and sends `EndOfStream`
   + revokes the registry via a `Drop` on its state), behind the `zmq` feature.
   The pluggable **discovery backend** is preserved as the skill's
   trait-behind-a-feature: a `ZmqRegistry` trait with `ZmqPubRegistration` /
   `ZmqSubConfig` `Into`-wrappers (`()` / bare-address vs `(name, registry)`),
   and an `EtcdRegistry` implementation gated on the `etcd` feature. Parity port
   of the classic tests: no-service wiring tests in `tests/zmq_adapter.rs`
   (`zmq`), real-socket pub/sub tests in `tests/zmq_integration.rs`
   (`zmq-integration-test`), and etcd-discovery tests in
   `tests/zmq_etcd_integration.rs` (`zmq-etcd-integration-test`, testcontainers);
   classic direct-mode example ported to `examples/zmq_adapter.rs`. Like classic,
   the `zmq` feature deliberately does **not** depend on `async`. **Deviations**
   (capabilities all preserved): (a) `zmq_sub` takes a `&GraphBuilder` and a
   `RunMode` and **rejects `RunMode::HistoricalFrom` at wiring time** — next's
   channel is bimodal and would block-collect the never-closing subscriber and
   deadlock at `start`, so it errors rather than rejecting at run start the way
   classic's realtime-only `ReceiverStream` does; (b) `zmq_pub` returns a sink
   `Stream<()>` (not `Rc<dyn Node>`), binding/registering/run-mode-checking at
   graph `start()` (before the first payload, so a fresh subscriber's filter
   propagates during the startup window rather than racing the first publish)
   and a historical run still errors with "real-time" before touching the
   registry; (c) the `bincode` wire envelope is next-local,
   so a next publisher interoperates with a next subscriber but is **not**
   wire-compatible with a classic/Python peer — cross-language interop lands with
   the Python bindings (Phase 6), which is also why the classic `zmq-cross-lang`
   tests are not ported. Realtime-only, so the parity tests assert received
   values (consecutive counters, connection status) rather than exact tick times.
   ✅ **kdb** *(done)*: KDB+/q — two time-partitioned historical replay sources
   (`kdb_read`, one query per time slice via the shared time slicer; and its
   file-cached twin `kdb_read_cached` over the `cache` adapter's `FileCache`), a
   real-time tickerplant subscription (`kdb_sub`), and a streaming insert sink
   (`KdbSinkOps::kdb_write`), behind the `kdb` feature on the async `kdbplus` IPC
   client (`kdb-plus-fixed` 0.5, mirroring classic). All ride the async
   ergonomics: `kdb_read`/`kdb_read_cached`/`kdb_sub` over `produce_async`,
   `kdb_write` over `consume_async`. As the *reusing* time-sliced adapter, it
   **widened the slicer cfg-gate** in `adapters::common` from
   `#[cfg(feature = "postgres")]` to `#[cfg(any(feature = "postgres", feature =
   "kdb"))]` (the `WindowFilter` row-clamp is always compiled; only the slicer is
   gated). Parity port of the classic adapter's + cache tests as
   `tests/kdb_integration.rs` (KDB+ has no public licensed container image, so the
   tests probe an external `q -p 5000` and **skip** when unreachable — no
   testcontainers; `kdb-next-integration.yml` reuses the classic adapter's KDB
   Docker image + license secret) plus no-service tests in `tests/kdb_adapter.rs`;
   the three classic examples ported to `examples/kdb/{read,read_cached,round_trip}`.
   **Deviations:** all classic capabilities preserved — the read/read_cached/sub
   sources, the write sink, the `KdbDeserialize`/`KdbSerialize`/`KdbExt` traits,
   `Sym`/`SymbolInterner`, and the `Row`/`Rows` access. After the defer-to-start
   and runtime-ownership migrations the graph owns the tokio runtime (no `&Handle`;
   register A5), the sink connects lazily on its first write (register A1/A4), and
   `kdb_read`/`kdb_read_cached` defer their connect + slice queries to the run via
   `produce_async` (the window is still validated + sliced at **wiring**, a pure
   check — register B5-style). `kdb_sub` takes a `RunMode` and **rejects
   `RunMode::HistoricalFrom` at wiring** (a live, unbounded tickerplant tail with
   no bounded historical twin — register B2, ratified; classic checked the same
   guard at run start). The sink is the `KdbSinkOps` extension trait (not the
   classic free-fn + `KdbWriteOperators` pair) and takes a `buffer_size` for the
   `consume_async` bound; `kdb_read` takes classic's `buffer_size`, now an
   effective bound (B5 lazified the slice replay, so `Some(n)` gives
   bounded-memory pipelined historical replay; `None` = unbounded, classic's
   default). Credentials never reach an error message
   (`KdbConnection::redacted()` = `host:port`). The canonical deviation list is the
   adapter's `# Deviations from classic` module-doc block plus
   [`deviation-register.md`](./deviation-register.md).
   ✅ **kafka** *(done)*: a streaming topic-consume source (`kafka_sub`) on
   `produce_async` and a topic-produce sink (`KafkaSinkOps::kafka_pub`) on
   `consume_async`, behind the `kafka` feature (`rdkafka` 0.37, mirroring
   classic). Parity port of the classic adapter's tests as
   `tests/kafka_integration.rs` (testcontainers/Redpanda, gated on
   `kafka-integration-test`; `kafka-next-integration.yml`) plus no-service tests
   in `tests/kafka_adapter.rs`, and the round-trip example (`kafka_adapter`).
   **Deviations:** all classic capabilities preserved; after the defer-to-start
   and concurrency migrations the graph owns the tokio runtime (no `&Handle`;
   register A5), `kafka_pub` connects lazily (librdkafka opens no socket until the
   first `send()`; register A1/A4), and — register B3 resolved — a burst's records
   are now produced **concurrently** via `consume_async_bursts` + `FuturesUnordered`
   (~one broker roundtrip/burst, order preserved across bursts, at throughput
   parity with classic) rather than sequentially. `kafka_sub` takes a `RunMode`
   and **rejects `RunMode::HistoricalFrom` at wiring** (a live, unbounded consumer
   with no historical timeline to replay; register B2, ratified until a bounded
   offset-range reader exists — classic technically permitted a wall-clock
   historical run). The canonical deviation list is the adapter's `# Deviations
   from classic` module-doc block plus [`deviation-register.md`](./deviation-register.md).
   - **`async` feature fix:** enabling `async` now also enables `tokio/sync`
     (which `consume_async`'s sink channels need). Previously the `async` feature
     compiled only because a companion adapter (`etcd-client`) pulled `tokio/sync`
     in transitively; `kafka` is the first async adapter that does not, so the
     feature is now self-contained.
6. **fix** — codec-heavy; fallibility with context.
   ✅ **fix** *(done)*: the FIX (Financial Information eXchange) protocol — a
   synchronous, poll-based session engine (initiator [`fix_connect`] / acceptor
   [`fix_accept`], plain TCP or TLS) exposing inbound messages + session status
   as streams, a market-data subscription helper ([`FixConnection::fix_sub`]),
   and an outbound sender ([`FixOperators::fix_send`] + the [`FixSender`] inject
   channel), behind the `fix` feature. Like classic (and unlike the async
   adapters) it uses **no** `async`/tokio: the hand-written FIX 4.4 tag-value
   codec, the `FixSession` state machine, and both poll modes are a verbatim
   port. **Both poll modes ported:** `FixPollMode::Threaded` rides
   `source_at_start` (a background session thread over the `channel` layer, with
   initiator reconnect-after-drop, acceptor re-accept, and the lock-free `kanal`
   inject channel drained per loop), and `FixPollMode::AlwaysSpin` rides a
   busy-spin `custom_node` (non-blocking socket reads on the graph thread) with
   the socket connect/bind deferred to graph `start()` (via
   `compose_spawn_at_start`) and a best-effort Logout at teardown. The pluggable
   `FixLogon` auth seam (None / Password / `custom` Ed25519-signer over the
   `LogonContext`) is preserved. **Engine fix (custom_node honours ALWAYS):** a
   busy-spin `custom_node` must set the engine's `has_always` flag so the realtime
   kernel doesn't park between cycles — `custom_node` accepted an `Activation` but
   ignored the `always` bit's kernel implication (only `poll` set it); now
   `custom_node` sets `has_always` when `activation.always`, so the spin source is
   actually driven every cycle (register A7). **Deviations** (all capabilities
   preserved): the source factories take a `&GraphBuilder` + `RunMode` and
   **reject `RunMode::HistoricalFrom` at wiring** (a live session has no historical
   timeline to replay; classic checked real-time-ness at run `start()` — register
   B2); the sources return `Stream`s (not `Rc<dyn Stream>`), `fix_send` returns
   `Result<Stream<()>>` and `fix_sub` a `Stream<()>` (checking real-time at
   `start()` like classic, aborting a historical run there); and the `Threaded`
   teardown drops classic's `Arc<Mutex<Option<TcpStream>>>` socket-shutdown handle
   for a stop-flag-against-the-read-timeout exit (the zmq pattern — no lock on the
   graph path; teardown costs up to one 200 ms read-timeout longer). No-service
   wiring tests in `tests/fix_adapter.rs` (`fix`); same-process socket round-trip +
   reconnect + connection-refused parity tests in `tests/fix_integration.rs`
   (`fix-integration-test`, real loopback, no container); classic `fix_loopback`
   example ported to `examples/fix_adapter.rs`. The credentialed LMAX-demo
   integration tests are **not** ported (external credentials). The canonical
   deviation list is the adapter's `# Deviations from classic` module-doc block
   plus [`deviation-register.md`](./deviation-register.md).
7. **web** (+ wingfoil-wire-types, wingfoil-wasm, wingfoil-js untouched —
   the wire protocol is engine-agnostic), **prometheus, otlp, augurs**.
   *"wingfoil-js untouched" holds only if the ported web adapter reproduces
   the v2 control plane the client depends on:* `Hello`/`Subscribe`/
   `Unsubscribe`, burst payloads (`Stream<Vec<T>>` published as an array), both
   codecs (`Bincode` + `Json` — the latency tracker requires `Json`), and —
   the one JS-facing behaviour riding on an **engine-side** trigger rather than
   the frozen wire format — `ControlMessage::Complete { topic }` emitted when a
   `web_pub` source stream *ends* (historical replay / finite `RunFor`). The
   client's `onComplete` and its "stop reconnecting after a clean finish" logic
   both hinge on `Complete`, so the port must plumb a "publish source finished"
   signal (adjacent to `Ctx::is_last_cycle` / lifecycle) through to the adapter,
   not just carry the wire types over.
   ✅ **web** *(done)*: bidirectional WebSocket streaming to browsers — the
   axum HTTP/WS `WebServer` (own thread + own current-thread tokio runtime,
   synchronous bind, optional static-file serving, optional `web-tls` HTTPS/WSS
   via rustls/`ring`), the publish sink (`WebSinkOps::web_pub` on any
   `Stream<T: Serialize>`, plus `WebBurstSinkOps::web_pub_bursts` on
   `Stream<Burst<T>>`) over `consume_async`, and the browser-input source
   (`web_sub`) over `produce_async`, behind the `web` feature. **The
   "wingfoil-js untouched" condition holds:** the wire format is the shared,
   engine-agnostic `wingfoil-wire-types` crate reused verbatim — v2 control plane
   (`Hello`/`Subscribe`/`Unsubscribe`), both codecs (`Bincode` + `Json`), burst
   payloads published as arrays, and **`ControlMessage::Complete { topic }`**
   emitted when a `web_pub` source finishes (historical replay / finite
   `RunFor`), which `@wingfoil/client`'s `onComplete` and its
   stop-reconnecting logic depend on. next plumbs that end-of-stream signal
   through the sink's **teardown**: `consume_async`'s `flush` is chained into a
   `finally` that drains every queued frame, joins the consumer, then broadcasts
   `Complete` — so the marker still lands strictly after the last data frame. Both
   historical shapes are ported: streaming a replay through a live `start()`
   server (with `web_sub` yielding an empty source so the run never blocks) and
   the `start_historical()` no-op server. Parity port of the classic adapter's
   in-process tests as `tests/web_adapter.rs` (`web`; the wss:// round trip and
   the rcgen cert fixture behind `web-tls-integration-test`;
   `web-next-integration.yml`) — 13 tests, no container; classic example ported to
   `examples/web/`. **Deviations:** all classic capabilities preserved; `web_sub`
   takes a `&GraphBuilder` and returns `Result`, and — unlike the live `_sub`
   sources of register B2 — **does not reject `RunMode::HistoricalFrom`**, because
   it is finite in that mode (an immediately-ending empty stream, exactly as
   classic); the sink is the `WebSinkOps` trait (not the classic free-fn +
   `WebPubOperators` pair) returning `Stream<()>`; and `WebBurstSinkOps::web_pub_bursts`
   is added so a `Stream<Burst<T>>` publishes an atomic same-instant array without
   the caller hand-mapping to `Vec<T>` (`Burst`/`TinyVec` is not `Serialize`, so it
   cannot be a second impl of the same trait). The canonical deviation list is the
   adapter's `# Deviations from classic` module-doc block plus
   [`deviation-register.md`](./deviation-register.md).
   ✅ **prometheus** *(done)*: a realtime, pull-based metrics **sink** —
   `PrometheusExporter` (owns the registry, spawns the hand-rolled `GET /metrics`
   HTTP server, synchronous bind) plus the `PrometheusSinkOps::prometheus_gauge`
   extension trait that registers a lock-free `arc-swap` slot per metric and
   wires the publish sink (over `register_op1`), behind the `prometheus` feature.
   No-op under historical replay (reads the new
   [`Ctx::run_mode`](../crates/wingfoil-next/src/op.rs) accessor). Self-contained
   parity tests in `tests/prometheus_adapter.rs` (the classic exporter unit tests
   + the `multiple_metrics` self-contained integration test, raw-TCP scrape); the
   end-to-end Prometheus-scrape test is `tests/prometheus_integration.rs` behind
   `prometheus-integration-test` (reuses the classic Docker stack;
   `prometheus-next-integration.yml`). **Deviations** (all capabilities
   preserved): (a) the sink is the `PrometheusSinkOps` extension trait
   (`stream.prometheus_gauge(&exporter, name)`), not an `exporter.register(...)`
   method, per the sink-as-trait convention; (b) `serve` returns
   `anyhow::Result<u16>` with `.context`, not `Result<u16, io::Error>`. The one
   engine addition is `Ctx::run_mode()` (a realtime-only IO sink needs to see the
   run mode; reported as `RealTime` inside an island, like `is_last_cycle`).
   ✅ **otlp** *(metrics + traces done)*: a realtime, push-based
   OpenTelemetry metrics **sink** — the `OtlpSinkOps::otlp_push` extension trait
   on any `Stream<T: Display>` exports each tick as an OTLP `f64` gauge over
   HTTP/protobuf, behind the `otlp` feature. Built on `consume_async` so the OTel
   SDK export (an `rt-tokio` 500 ms `PeriodicReader`) runs off the graph thread;
   the meter provider is built lazily in that task and dropped at teardown to
   flush (matching classic's drop-not-`shutdown()`). No-op under historical
   replay (reads `Ctx::run_mode()`, so no provider is built and no network calls
   are made). Self-contained parity tests in `tests/otlp_adapter.rs` (classic
   `push` unit tests: historical no-connect + bad-endpoint-graceful); the
   end-to-end export test is `tests/otlp_integration.rs` behind
   `otlp-integration-test` (a testcontainers OTel collector;
   `otlp-next-integration.yml`). **Deviations:** all classic capabilities
   preserved; after the runtime-ownership migration the graph owns the tokio
   runtime (register A5), so `otlp_push` takes **no** `&Handle` —
   `stream.otlp_push(name, config)` — and the sink is the `OtlpSinkOps` extension
   trait rather than a classic `OtlpPush` on `dyn Stream<T>` (the graph must be
   driven from a non-async thread, the `consume_async` footgun). **Trace/span
   export ✅ ported** (`OtlpSpanOps::otlp_spans`, register C1 resolved): emits one
   parent span per tick plus one child span per stage hop from
   `Stream<P: HasLatency>` values (now that the Phase 5 latency infrastructure
   has landed), with caller-supplied attributes via `OtlpAttributeBuffer` and
   the silent skip of all-zero / backwards timestamps. Same off-thread
   `consume_async` model as `otlp_push` (the tracer provider is built lazily on
   the first exported value and dropped at teardown to flush; no-op under
   historical replay); note the span sink's argument order differs from classic —
   next `otlp_spans(span_name, config, attrs)` vs classic
   `otlp_spans(config, span_name, attrs)`. The canonical deviation list is the
   adapter's `# Deviations from classic` module-doc block plus
   [`deviation-register.md`](./deviation-register.md). Parity tests:
   `spans_historical_mode_drains_without_connecting` in `tests/otlp_adapter.rs`
   and `otlp_spans_sends_successfully` in `tests/otlp_integration.rs`.
   ✅ **augurs** *(done)*: on-graph time-series analysis (a pure-Rust compute
   adapter, no service/lifecycle), behind the `augurs` feature. Ports **all 6 of
   classic's operators** — `AugursForecastOps::augurs_forecast` (windowed ETS /
   MSTL point forecast + prediction intervals),
   `AugursOutlierOps::augurs_outlier` (windowed MAD / DBSCAN multi-series outlier
   detection), `AugursChangepointOps::augurs_changepoint` (Bayesian online
   changepoint detection), `AugursSeasonsOps::augurs_seasons` (periodogram
   seasonality detection), `AugursDtwOps::augurs_dtw` (pairwise dynamic-time-warping
   distance matrix) and `AugursClusterOps::augurs_cluster` (DBSCAN clustering over
   those DTW distances) — all as sliding-window transform ops computing inside
   `cycle()` on the graph thread (same shape as the `stats` rolling ops).
   **Deviations:** the ops validate config inside `cycle` (returning `Result` /
   `anyhow::bail!`) rather than classic's wiring-time `panic!` on a bad detector
   sensitivity, and `augurs_cluster` floors its effective window at the
   two-sample warm-up (classic's cluster node never ticks for `window == 1`) —
   both deliberate improvements; see the adapter's `# Deviations from classic`
   module-doc block plus [`deviation-register.md`](./deviation-register.md).
   Test file `tests/augurs_adapter.rs`; example `examples/augurs_adapter.rs`.
8. **aeron, iceoryx2, fluvio** last — build-environment pain (CMake/clang);
   their ring-buffer polling is the natural `ALWAYS`-cap shape.
   ✅ **fluvio** *(done)*: a streaming topic-partition consume source
   (`fluvio_sub`) on `produce_async` and a topic-produce sink
   (`FluvioSinkOps::fluvio_pub`) on `consume_async_bursts`, behind the `fluvio`
   feature (`fluvio` 0.50.1, mirroring classic). Unlike aeron/iceoryx2 it is an
   ordinary async network client, so it needs no native toolchain and lands
   first of the three. Parity port of the classic adapter's tests as
   `tests/fluvio_integration.rs` (testcontainers, `infinyon/fluvio:0.18.1` with
   host networking + the SC/SPU registration dance, gated on
   `fluvio-integration-test`; `fluvio-next-integration.yml`) plus no-service
   tests in `tests/fluvio_adapter.rs`, and the classic round-trip example ported
   to `examples/fluvio/`. **Deviations:** all classic capabilities preserved
   (offset-selected partition consumption, keyed/keyless records, per-burst flush
   batching, the single-record convenience sink); the graph owns the tokio
   runtime (no `&Handle`; register A5), `fluvio_pub` connects + creates its
   producer lazily on the first burst inside `consume_async_bursts` (register
   A1/A4), and `fluvio_sub` takes a `RunMode` and **rejects
   `RunMode::HistoricalFrom` at wiring** (a live, unbounded consumer that tails
   forever after the retained records, with no historical timeline to replay;
   register B2, ratified — classic technically permitted a wall-clock historical
   run). The sink is the `FluvioSinkOps` extension trait (not the classic free-fn
   + `FluvioPubOperators` pair) and takes a `buffer_size` for the
   `consume_async_bursts` bound; a negative `start_offset` is rejected at wiring
   rather than deferred into the producer future. The canonical deviation list is
   the adapter's `# Deviations from classic` module-doc block plus
   [`deviation-register.md`](./deviation-register.md).

   ✅ **aeron** *(done)*: the Aeron IPC/UDP low-latency message transport, behind
   the `aeron` (rusteron-client, C++ FFI — production) or `aeron-rs` (pure Rust —
   experimental) backend features, with `aeron-driver` embedding a media driver.
   Versions mirror classic. **Both polling modes ported:** `AeronMode::Spin`
   rides a busy-spin `custom_node` polling on the graph thread, and
   `AeronMode::Threaded` rides `source_at_start` (a background poll thread over
   the `channel` layer with classic's exponential idle back-off). The typed
   parser with `FragmentHeader` access, the `fragment_limit` per-poll cap, the
   `Spin`→`Threaded` downgrade for backends that lock on poll, both status
   side-channels, the `ChannelUri` builders, `ClaimBuffer`'s zero-copy
   commit/abort contract, and the `TransportError`/`AeronStatus` types are all
   ported. Like classic it uses **no** `async`/tokio. Parity port of the classic
   node-level unit tests as `tests/aeron_adapter.rs` (`aeron` — 20 mock-backed
   tests, no media driver) plus the classic integration suite as
   `tests/aeron_integration.rs` (`aeron-integration-test` — a testcontainers
   `neomantra/aeron-cpp-debian` media driver bind-mounting `/dev/shm`;
   `aeron-next-integration.yml`, which also installs the cmake ≥3.30 / clang /
   uuid / libbsd toolchain rusteron needs); both classic examples ported to
   `examples/aeron/`. **Deviations:** all classic capabilities preserved; the
   sources take a `&GraphBuilder` + `RunMode`, return `Result`, and **reject
   `RunMode::HistoricalFrom` at wiring** (a live Aeron subscription has no
   historical timeline, and the threaded mode's channel receiver would
   block-collect the never-closing poll thread and deadlock at `start` — register
   B2, ratified; classic's spin subscriber silently ran against the
   fast-forwarded historical clock, while the *publisher* keeps classic's
   real-time check at graph `start()`). The **status side-channel is a plain
   stream, not a node type**: classic's `AeronStatusStream` (a `MutableNode` the
   producer drove via `clear()`/`record()` and wired as an active downstream) has
   no next twin — next multiplexes status with data over one internal envelope
   and splits it with `map_filter`, the `zmq_sub` shape, so the *spin* mode now
   carries status in-band too. Observable behaviour (transition-only emission,
   derivation order, in-band ordering) is identical. The sink is the
   `AeronSinkOps` extension trait returning `Stream<()>` (not classic's
   `AeronPub` returning `Rc<dyn Node>`), and the `MockSubscriber`/`MockPublisher`
   backends are public test support (next's tests live outside the lib). The
   classic Criterion benches (`aeron_publication_latency`,
   `aeron_subscription_throughput`, `aeron_transceiver`,
   `aeron_allocation_tracking`) are **not** ported — next's bench suite is a
   separate work item, as for every adapter so far. The canonical deviation list
   is the adapter's `# Deviations from classic` module-doc block plus
   [`deviation-register.md`](./deviation-register.md).
   ✅ **iceoryx2** *(done)*: zero-copy inter-process (and intra-process)
   publish/subscribe over shared memory, behind the `iceoryx2` feature
   (`iceoryx2` 0.8, mirroring classic). Pure Rust — unlike aeron it needs no
   native toolchain. **All three classic polling modes ported:**
   `Iceoryx2Mode::Spin` rides a busy-spin `custom_node` (port creation deferred to
   graph `start()` via `compose_spawn_at_start`, draining the subscriber port into
   one burst per cycle), and `Threaded`/`Signaled` ride `source_at_start` (a
   background thread over the `channel` layer — a 10 µs-yield poll loop, or a
   blocking `WaitSet` attached to the service's `<name>.signal` Event service).
   Both the typed (`ZeroCopySend`) and `[u8]` slice APIs are ported, in both the
   `Ipc` and `Local` service variants, with the full classic constructor family
   (`_sub`/`_sub_with`/`_sub_opts`, `_sub_slice`/`_sub_slice_opts`), the service
   contracts, `FixedBytes<N>`, and the typed `Iceoryx2Error`. Like classic (and
   unlike the networked adapters) it uses **no** `async`/tokio. Parity port of the
   classic `local_tests.rs` as `tests/iceoryx2_adapter.rs` (`iceoryx2` — the
   in-process `Local` round trips in all three modes, typed and slice, the
   contract-mismatch case, and the `Traced` latency round trip across an iceoryx2
   hop) plus the classic `integration_tests.rs` as `tests/iceoryx2_integration.rs`
   (`iceoryx2-integration-test` — cross-process `Ipc` over real `/dev/shm`, no
   container; `iceoryx2-next-integration.yml`); the two classic examples ported to
   `examples/iceoryx2/{pub,sub}.rs`. **Deviations:** all classic capabilities
   preserved; the sources take a `&GraphBuilder` + `RunMode`, return `Result`, and
   **reject `RunMode::HistoricalFrom` at wiring** (a live shared-memory
   subscription has no historical timeline, and the `Threaded`/`Signaled` channel
   path would block-collect the never-closing producer and deadlock at `start` —
   register B2, ratified; classic silently ran its poll loop against the
   fast-forwarded historical clock); the sinks are the `Iceoryx2SinkOps` /
   `Iceoryx2SliceSinkOps` extension traits returning `Stream<()>` (not the classic
   free-fn family returning `Rc<dyn Node>`); and — deliberate classic parity — the
   **sink does not reject or no-op under historical replay**, unlike `zmq_pub`
   (which errors) and the telemetry exporters (which no-op). Ports are created at
   graph `start()` as in classic, so wiring is pure and a bad service name or
   contract mismatch aborts the run with node context (register A1/A4). The
   classic Criterion benches (`iceoryx2`, `iceoryx2_modes`) are **not** ported —
   next's bench suite is a separate work item, as for every adapter so far. The
   canonical deviation list is the adapter's `# Deviations from classic`
   module-doc block plus [`deviation-register.md`](./deviation-register.md).

Each adapter: keep its directory CLAUDE.md, port its tests, one PR each.

**Gate 4:** adapter test suites green on next; classic adapter code paths
untouched (still shipping) until Phase 7.

## Phase 4.5 — engine execution model: breadth-first dirty-list parity

**Scheduling: ✅ landed.** The interpreted engine now runs a sparse dirty-list
by default (`interp.rs`, `Dispatch::Sparse`), reproducing classic wingfoil's
`dirty_nodes_by_layer` model. Each cycle seeds a work set from the frontier —
`always` busy-poll ops plus kernel-marked callback-activated ops (tickers,
`delay` pops, the feedback source, channel replay) — then propagates the tick
frontier forward: a node that ticks marks its active downstream neighbours
dirty. The work set drains in ascending **`(layer, index)`** order — `layer[i]`
is the longest path to `i` over active *and* passive edges (classic's layer
order); it collapses to plain index order on a static graph (a valid
topological order, since the fluent API forces a stream to exist before it is
referenced), so each node fires exactly once after everything it reads —
glitch-free, and with per-cycle work proportional to the nodes that *actually
fire*, not the graph size `N`. Results are **byte-identical** to classic and to
the old `O(N)` full-index sweep, which is retained as `Dispatch::FullSweep` — an
executable reference oracle (`runner.with_dispatch(Dispatch::FullSweep)`) the
parity suite can cross-check against. This closes the sparse-graph performance
gap against classic, gated by `sparse_work_is_independent_of_graph_size`
(`tests/sparse_graph.rs`) and measured by `benches/store_baseline.rs`.

**Dynamism: ✅ landed** (behind the `dynamic-graph` feature). The `(layer,
index)` key is what makes runtime mutation possible — a node appended at the
highest index can be spliced beneath an existing lower-indexed caller, with
`fix_layers` lifting the caller's layer above the new upstream (the reorder
plain index order cannot express). Surfaces: `Runner::run_dynamic` + an
`Extension` scope (append / active-passive splice / remove / recycle), an
in-graph `Builder::dynamic_group` (classic's `dynamic_group_stream` twin) that
stages insert/remove from its own `cycle`, and `Builder::demux` (fixed-topology
routing on a same-cycle mark-dirty primitive — no add/remove). Removed slots are
tombstoned, not freed (classic parity). Parity tests in
`tests/dynamic_graph.rs`.

**Known parity gaps (for the cutover audit).** The runtime *behaviour* is a
faithful twin (values + tick times match classic's oracles). The two
dynamic-graph ergonomic gaps below are now **closed** (both were ergonomic
surface over the existing mechanism — no engine/staging changes):

- **`StreamStore` (pluggable `dynamic_group` backing store). ✅ closed.**
  `Builder::dynamic_group_with_store` takes a caller-supplied
  [`StreamStore`]-implementing container for the group's live members; the
  `StreamStore` trait (one value type param) has blanket impls for `BTreeMap`
  (`K: Ord`) and `HashMap` (`K: Hash + Eq`). `dynamic_group` is unchanged — it
  delegates with a `BTreeMap`, still the default (deterministic iteration is the
  safer backtest default). The added capability is a `Hash + Eq` key that is not
  `Ord`; the container is otherwise irrelevant to cost (per-cycle work iterates
  *live* members, O(members), regardless of backing store). Parity test:
  `dynamic_group_with_store_supports_non_ord_hashmap_key` in
  `tests/dynamic_graph.rs`.
- **`DemuxMap` key lifecycle + `demux_it`. ✅ closed.** `Builder::demux` stays
  the raw routing primitive (`route(value) -> slot` + overflow). Layered on top
  of it (no engine changes): `Builder::demux_map` (twin of classic
  `StreamOperators::demux`) adds the auto-assigning / `Close`-releasing
  `DemuxMap` key lifecycle over a single value, and `Builder::demux_it` (twin of
  classic `demux_it`) routes each item of an iterable source value to its keyed
  child, each selected child re-emitting a `Burst` of exactly its items. next's
  `DemuxMap` assigns the *lowest* free slot (a `BTreeSet` pool rather than
  classic's `HashSet`), so slot assignment is deterministic. Parity tests:
  `demux_map_auto_assigns_and_releases_slots` and
  `demux_it_routes_each_item_to_a_burst_per_child`.

### The perf gate — a test, not a benchmark

The dirty-list's headline claim is that per-cycle work tracks the *active* node
count rather than the graph size `N`. That claim is now pinned by
**`sparse_work_is_independent_of_graph_size`** (`tests/sparse_graph.rs`), which
runs in CI with every other test.

Benchmarks were the obvious home for it and are the wrong one: nothing in
`.github/workflows/` runs `cargo bench`, and a wall-clock ratio is a reading,
not a pass/fail. The test instead measures the property exactly, via
`Runner::node_visits()` — a count of nodes the dispatch loop had to look at,
fired or not, accumulated once per cycle (`O(1)`, free on the hot path).

The invariant needs one piece of care: every ticker is due at `t = 0`, so cold
padding does genuinely fire once, and no amount of padding is *free* outright.
The gate is therefore that padding costs a **one-off, never a per-cycle tax** —
it measures what the padding adds at two run lengths 10x apart and asserts the
two deltas are equal. Measured today: 504 extra quiet branches cost the sparse
engine **1,008 visits regardless of run length** (the `t = 0` activation, once),
while the `FullSweep` oracle pays those same 1,008 *every cycle* — 201,600 over
a 200-cycle run. The oracle is asserted on too, so the gate cannot degrade into
a tautology: if `node_visits` ever stopped being sensitive to graph size, the
oracle half of the test fails first.

### The `O(depth)` drain term — ✅ fixed

Measuring the tiers on sparse workloads (`benches/tiers.rs`, groups `sparse` /
`sparse_wide`) turned up a real qualifier on the "work ∝ active nodes" claim.
Node *count* was already free — padding a graph with dangling quiet branches
costs nothing measurable, which is what the gate above pins. But the drain loop
walked `0..=max_layer` **testing every bucket**, so per-cycle cost was really
`O(active + deepest active layer)`, and a graph that is merely *deep* paid for
its depth on every cycle even when almost none of it fired.

Depth is not an exotic shape here: next's `fan` sugar **left-folds its branches
into a binary merge chain**, so a 256-way fan-in is a ~256-deep graph (classic's
`merge(vec)` is a single N-ary node, depth 1, which is why classic never showed
the term). It also needs an *active* node above the quiet depth to bite — a join
like `hot.merge(&deep)` — since otherwise `max_layer` never rises.

**Fixed** (`interp.rs`): the drain now finds occupied layers through an
`occupied` bitmask — one bit per layer, set on enqueue, cleared on drain — so one
word test skips 64 empty layers, leaving `O(active + depth/64)`. Deliberately a
bitmask rather than a heap of occupied layers: a heap costs a push/pop *per
layer*, and on a linear chain every layer holds one node, which would reintroduce
exactly the per-node heap traffic whose removal closed the `fanout` gap against
classic. The mask adds one branchless bit-set per enqueue and nothing per node.

Results: the marginal scan cost of 318 quiet layers fell from **636 steps per
cycle to 10**, and the benchmark's depth slope from **+63%** (2.70ms → 4.39ms
across the two padding widths) to **+8%** (2.94ms → 3.18ms). Gated by
`quiet_depth_does_not_cost_per_cycle` (`tests/sparse_graph.rs`), which fails at
636 against a bound of 39 if the walk returns.

### The missing n-ary merge — a real Phase 6 gate violation ⚠️

Chasing the depth term turned up its root cause, which is a **parity gap, not a
tuning opportunity**: next has no n-ary merge node. `merge_all` and `fan` both
unroll to a left-associated chain of binary `merge2`s (deliberately — "closes the
n-ary-merge vocabulary gap without a bespoke variadic op"), so an n-way fan-in
costs next `n-1` nodes where classic's `merge(vec)` costs **1**.

On a *busy* fan-in — every branch ticking every cycle, the common case — that is
a straight loss against classic, and it widens with width (20k cycles):

| width | next (chain) | next (balanced tree) | classic | chain/classic |
|-------|--------------|----------------------|---------|---------------|
| 16    | 14.2ms       | 10.9ms               | 9.8ms   | **1.45x**     |
| 64    | 48.3ms       | 41.8ms               | 28.0ms  | **1.73x**     |
| 256   | 194.4ms      | 157.2ms              | 104.7ms | **1.86x**     |

This violates the Phase 6 gate **`next-interpreted ≥ classic-interpreted`**. The
`fanout` benchmark misses it because it is only 10 wide, where the 9 extra merge
nodes are lost among ~105 others — the loss needs width to show.

**A balanced merge tree is not the fix.** The third column measures it: `O(log n)`
depth recovers roughly 40% of the gap (1.86x → 1.50x) and stops there, because
the remaining cost is the `n-1` extra *nodes*, which rebalancing does not remove.
Depth and node count are separate halves and only a real n-ary node removes both.

The fix is therefore a genuine variadic merge. The engine side is easier than the
`merge_all` doc implies: `push_node` already takes `Vec<usize>` upstreams, so
arbitrary arity is native and only the registration closure is hardcoded to two
slots. The work is a `merge_n` in `interp.rs` capturing `Vec<SlotRef<T>>`, the
fluent `merge_all`/`fan` redirected onto it, exact tie-break parity tests
("first supplied that ticked wins", plus burst semantics), and the harder part —
teaching `graph!`/compiled emission to emit an n-ary node instead of a chain.
Worth a widened `fanout` benchmark bar at the same time, so the gate that missed
this can catch it next time.

### Tier ranking on sparse graphs: no crossover, but `nested` inverts

The Phase 6 tier claim — compiled and nested beat the interpreters — was
established only on *dense* workloads, where every node fires every cycle. The
sparse groups now test the other regime, and the headline result is that
**compiled still wins outright**: ~705µs vs interpreted's ~2.70ms at 267 nodes,
~755µs vs ~4.39ms at 1035. There is no crossover where the dirty-list overtakes
straight-line emission, even at ~97% quiet — a per-node `__dirty[i]` predicate
is simply much cheaper than a dynamic dispatch, so idle nodes are nearly free to
walk. This also *weakens the case for compiled-path region gating* (branch-1's
idea, noted under "Scope notes" below): the cost it would remove measures as
small.

What does invert is **`nested`, which loses to plain interpreted on sparse
graphs** (~3.79ms vs ~2.70ms) — the mirror image of its dense win. An island
runs its whole compiled interior on every outer activation, so a mostly-quiet
interior is its worst case. Worth knowing before recommending islands as a
general accelerator: they pay off in proportion to how *busy* the interior is.

**Two follow-ons remain, both deliberately separated from the scheduler:**

### Arena / SoA value store — deferred perf follow-on (boundary frozen by type)

**Decision: do the arena later, not now.** It's a pure perf follow-on; the
interpreted engine is already at parity with classic (the dirty-list did that),
and nothing correctness- or cutover-related depends on it. The critical path to
cutover is *breadth* — catalog, adapters, facade, python — so the arena waits
for a measured need. Two cheap de-risking steps were done now instead (below),
so "later" stays free and evidence-driven.

Moving the per-slot `Rc<RefCell<T>>`s to a contiguous arena / structure-of-arrays
store is a **pure memory/throughput optimisation** (semantics unchanged) that
lands the "dense hot-path speed" number and enables the zero-copy passthrough
(a node that provably forwards its input aliases the upstream slot handle
instead of cloning — see the ref/aliasing note below).

The rework-trap the review flagged — every `Builder` registration closure
captures the concrete slot type, so a naive swap touches the whole ported
catalog + adapters **twice** — is **resolved: the slot API boundary is now
frozen by type.** ✅ `SlotRef<T>` (`interp.rs`) is the sole access boundary:
`slot()`/`new_slot()` return it, and every registration closure reads/writes
only through `SlotRef::borrow`/`borrow_mut`, never the concrete cell. Today it
wraps an `Rc<RefCell<T>>`; the arena becomes an internal swap of that innards
with **zero capture sites touched**. Audited: the macro crate never names the
slot type (compiled uses locals), and the one other hook —
`Stream::__slot` for `nested` islands — now returns `SlotRef<T>` too. So the
bulk catalog/adapter port proceeds against the frozen boundary and the arena
lands whenever a measured need appears.

**Measured baseline** (`benches/store_baseline.rs`). The go/no-go is now
bracketed by numbers rather than the earlier ~1.1–1.5× estimate (all ratios are
machine-dependent — regenerate before deciding, but the *shape* is the point):

- **Ceiling** (`forward_clone`, 8 KiB `Vec` through 16 `filter` hops): the
  owned-`Vec` run is **~7.4×** an `Rc<Vec>` run of the identical graph (≈20.8 ms
  vs ≈2.79 ms). The per-hop clone tax slot-aliasing would remove is *large* for
  big-payload forwarding — the arena+aliasing has real teeth **there**.
- **Floor** (`forward_scalar`, the same shape with an `f64` payload): ≈1.84 ms —
  *faster* than even the `Rc<Vec>` path, because a scalar clone is a register
  copy. Aliasing the slot recovers **nothing** here; the only lever left on
  scalar graphs is SoA cache locality (not isolated by this bench).

So the realistic per-graph win lives *between* this floor and that ceiling,
weighted by how much payload the graph actually forwards by clone. Typical
wingfoil workloads (scalars, small structs, statistics) sit near the floor —
**evidence for keeping the arena deferred until a real big-payload-forwarding
graph appears.** The same bench's `sparse_dispatch` group measures the
dirty-list's win in wall-clock: `Dispatch::Sparse` runs **~7.3×** faster than
the `FullSweep` oracle (≈11.2 ms vs ≈81.6 ms) on a graph padded with cold nodes.
The pass/fail *gate* for that claim is the test described above, not this
benchmark — nothing in CI runs `cargo bench`.

### Dynamic graphs (runtime add/remove) — ✅ landed

The sparse dirty-list maintains a mutable frontier of active nodes — the
natural home for classic's `graph_node` / `dynamic_group` (add/remove nodes
and sub-graphs mid-run), which the old all-nodes sweep could not cleanly
express. The enabler *and* the feature are now both in place, behind the
`dynamic-graph` cargo feature: `Runner::run_dynamic` appends nodes + slots and
splices active/passive edges at runtime (with `fix_layers` updating the layer
bookkeeping for the affected region), plus the in-graph `Builder::dynamic_group`
(classic's `dynamic_group_stream` twin, with a pluggable `StreamStore`) and the
`Builder::demux` routing primitive (`demux_map` / `demux_it` layered on top).
Parity tests in `tests/dynamic_graph.rs`. The open *decision* the plan flagged —
is runtime mutation a cutover blocker or a v1 deviation — is thereby settled by
building it: it is supported, matching classic. The compiled and island paths
stay static by design (their whole value is a fixed monomorphized schedule);
dynamism is an interpreted-engine capability, matching classic. See the Phase
4.5 header and capability-matrix note ¹⁰ for the surface detail.

**Scope notes:**
- The scheduler change was pure mechanism/performance — observable results
  stayed identical, so the full existing parity suite (catalog, macro,
  feedback, channel) plus the `FullSweep` oracle guard it. The perf-parity
  claim itself is now gated too — see "The perf gate" above.
- The **compiled**/island path is unaffected in shape (it already emits
  straight-line per-node dispatch, the static-schedule analogue), but this is
  where branch-1's *region gating* idea (skip whole quiet sub-graphs) becomes
  the compiled counterpart of the dirty-list — worth doing alongside the
  arena/perf pass.
- Bench gate ties to Phase 6: `next-interpreted ≥ classic-interpreted` on the
  sparse workloads holds — `benches/tiers.rs` measures ~2.70ms vs classic's
  ~3.23ms on the `sparse` group (and next wins the dense groups too).

## Phase 5 — infrastructure

- **Latency** ✅ **landed** (`src/latency.rs`): stamps ride values as today
  (`Traced` is just a payload, re-exported from classic together with
  `Latency`/`Stage`/`HasLatency`/`StageStats`/`LatencyStats` and the
  `latency_stages!` derive — all engine-agnostic, unchanged). `Ctx` gained
  `wall_time()` (a per-cycle snap, from a new `Kernel::wall_time`) and
  `wall_time_precise()` (fresh TSC read); the node layer is re-implemented as
  ops — `stamp`/`stamp_precise` (over `register_op1`) and the `latency_report`
  sink — exposed via the `LatencyStreamOps`/`LatencyReportOps` fluent traits.
  **Deviation**: fluent/interpreted only (matching classic, which exposes
  latency solely through `LatencyStreamOps`); a stamp's stage is a compile-time
  *type* parameter, which does not map onto the `graph!` value-dispatch table,
  so compiled/nested support is out of scope for this op family.
- **Graph export**: ❌ **not doing this** (GML from `Builder` topology + debug
  labels). Deferred deliberately — we want a better introspection/visualization
  story than a one-off GML dump, to be designed and scoped separately later
  rather than ported as-is from classic.
- **`#[node]` retirement** ✅ **done in next**: replaced by `Op` impls. There
  is no `#[node]`, and no dependency on `wingfoil-derive`, anywhere under
  `next/` — every node in the catalog, the adapters, and the tests is an `Op`
  impl (semantics as associated functions, `Cfg`/`State`/`In`/`Out`), which is
  what let one body serve all three engines. The user-facing escape hatch
  `#[node]` existed for — writing a node by hand — is
  `GraphBuilder::custom_node` plus the public `register_op1`…`register_op4`
  primitives (`tests/custom_node.rs`, `tests/custom_op.rs`). Deleting the
  `wingfoil-derive` *crate* belongs to the cutover, when the legacy tree it
  serves is removed; nothing in next blocks it.
- **`#[op]` tooling** ✅ **landed**: `#[op(build = name)]` generates the
  interpreted `Builder` method *and* the `graph!`/compiled forwarders from one
  attribute, with labels derived from `type_name`; there is no per-op table in
  the macro (see `macro-extensibility-decision.md`). See **Adding an op**
  under Phase 2. The completeness test guarding against one-sided registration
  ✅ landed in Phase 1 as a compile-guard (`tests/op_completeness.rs`) — see
  **Adding an op**.
  - **Shape coverage ✅ complete.** The generated `Builder` method used to be
    scoped to the single-active-input shape, so ~13 ops kept a hand-written
    method that repeated the same twenty-line wiring. It is now derived from
    the op's `In` shape and covers all of them: sources (`In = ()`),
    multi-input, tick-flag edges (`(&'a T, bool)`), `passive = [..]` masks,
    `start`/`stop`/`teardown` hooks, and `init_arg` seeded accumulators —
    `ticker`, `constant`, `throttle`, `window`, `timed`, `filter`, `fold`,
    `sample`, `finally`, `join`, `delay`, `merge` and `delay_with_reset` all
    dropped their hand-written wiring, and `try_join` / `join3` / `try_join3` /
    `join_passive` / `try_join_passive` gained generated methods the fluent
    layer now wires through. `no_builder` survives for one op, `with_time`,
    whose signature deliberately differs from its shape (see **Adding an op**).
    Per-shape tests in `tests/op_builder_shapes.rs`.
  - Still open (deliberate): generating the **fluent** method too. Only clean
    as inherent-on-`Stream`, which would close the open op vocabulary that the
    extension-trait design exists to keep — deferred, not owed.

## Phase 6 — Python bindings, examples, benches

**Decision (2026-07): `wingfoil-next-python` supersedes the legacy
`wingfoil-python` bindings — it is not a compatibility facade over them.** The
go-forward Python surface is the fresh object-form binding in
`next/crates/wingfoil-next-python` (`PyGraph`/`PyStream` over the shared
interpreted `GraphBuilder`, erased to `PyElement`, plus the
`#[pyop]`/`pyop_fn!` plugin seams — see `docs/python-interop.md`). Legacy
`wingfoil-python` (`import wingfoil`) is **retired at cutover**, not kept
running unchanged, so this is a **breaking change** for Python users
(`import wingfoil` → `import wingfoil_next`; next-python likely claims the
`wingfoil` module name via a rename at cutover). The gate is next-python's own
pytest suite (`test_interop.py`) reaching parity with the surface the legacy
tests covered — not "legacy pytest passes unchanged."

- **Object form** ✅ *landed*: `wingfoil-next-python` `PyGraph`/`PyStream`
  (`graph.rs`) — the "true `Rc<dyn Stream>` object form" this plan previously
  listed as remaining facade work — with `PyElement` erasure, re-runnable
  graphs, and the `#[pyop]`/`pyop_fn!` op-authoring seams.
- **Custom-node seam** ✅ *landed*: the public `GraphBuilder::custom_node`
  primitive (the next twin of classic `MutableNode` + `StreamPeekRef`,
  `tests/custom_node.rs`) plus its next-python exposure (`Graph.custom_node`,
  a `cycle(values) -> bool` + `peek()` protocol), so a Python object can be a
  graph node (legacy's `CustomStream`). Single-run in v1 (caller-owned Python
  state has no engine reset hook); next-python *regular* graphs re-run.
- **Surface build-out** 🟡 *in progress*: grow `PyStream`/`PyGraph` to cover
  the legacy combinator surface (`fold`/`sample`/`count`/`limit`/`difference`/
  `with_time`/`collect`/`buffer`/`window`/`not`, a `sum`/`mean` statistics
  bridge), then the per-adapter Python bindings as each Rust adapter lands.
- **Per-adapter Python bindings** 🟡 *mechanical + stream-transform tiers landed (9 of 15)*: the `#[pyadapter]`
  exposure of the real `adapters::*` I/O adapters, each behind a
  `wingfoil-next-python` cargo feature of the same name (`crate::adapters::*`,
  registered in the `#[pymodule]` under the same `#[cfg]`). **postgres** is the
  first and the template: `postgres_read` / `postgres_sub` / `postgres_source` /
  `postgres_write` / `postgres_notify_trigger_sql`, with a dynamic row↔`dict`
  edge and declared-column write marshaling, unit-level marshaling tests, and a
  service-backed pytest leg in `postgres-next-integration.yml`. Landing it
  closed three gaps in the seam itself, now available to every adapter that
  follows: `#[pyadapter]` accepts **fallible** wiring (`Result<Stream<T>>` → a
  `PyResult` fn, so a wiring rejection raises a Python exception), it **forwards
  `#[pyo3(signature = …)]`** so optional args get Python defaults, and
  `PyGraph::run` now **releases the GIL** for the run — without which no
  real-time adapter source can deliver, since its worker (and every other Python
  thread) stays blocked until `run` returns. It also grew a **free-fn form**
  (receiver as the first param) so a binding needs no throwaway trait and no
  duplicated signature, and the shared run-shape helpers live in
  `crate::adapters::common` (`historical_params` / `realtime_params` /
  `run_mode` / `secs_to_nanotime`) for the mode-aware sources that follow.
  The recipe now lives in its own skill, **`/bind-adapter-next`**, extracted
  from the Python step of `/new-adapter-next`.

  **kafka** followed as the first of the mechanical tier: `kafka_sub` /
  `kafka_pub`, a `KafkaEvent`↔`dict` read edge (`From<KafkaEvent> for
  PyElement`, so the source needs no intermediate type) and dict-to-
  `KafkaRecord` write marshaling with an optional `topic` fallback. It also set
  the rule that **`next-python-test.yml` builds the module with
  `-F all-adapters`**, since each binding's service-free pytest tier only runs
  if the module carries that adapter.

  **The wheel ships every adapter.** An earlier pass kept kafka/etcd/fluvio/zmq
  out of `[tool.maturin] features` on build-time grounds, which was wrong: a
  published wheel is the only copy a user gets, so an omitted adapter is simply
  absent from the module and their only recourse is a from-source build with a
  Rust toolchain — while the build cost is paid once, in the release job. The
  criterion that *does* justify an exclusion is **portability** (a system
  library that cannot be vendored, or a platform-specific wheel), which is
  exactly where legacy `wingfoil-python` draws it: everything ships except
  aeron and iceoryx2. Those two are the expected exclusions when they are
  bound.

  **redis** followed, and factored the sink-side marshaling out: `RecordDict`
  in `crate::adapters::common` is the record-`dict` reader every sink binding
  needs (a required payload, an optional key, a name with a caller-supplied
  fallback), each accessor failing loudly. kafka was migrated onto it in the
  same PR.

  **etcd** added `RecordDict::str` (a *required* str field, for a record whose
  target has no binding-level fallback) and the str-or-list endpoints form that
  exposes the engine's cluster support.

  **fluvio** added `RecordDict::opt_str`, and needed the most CI work of the
  six: a Fluvio cluster cannot be started with a single `docker run` — the SC
  must be told about the SPU *before* the SPU process connects, or it closes the
  connection. Its Python leg mirrors the sequence `tests/fluvio_integration.rs`
  documents (start the SC, `fluvio cluster spu register`, exec the SPU into the
  same container) on the fixed host-network ports that harness pins, and brings
  the cluster up *after* the Rust tests so the two never contend for them.

  **csv** is the first binding to need an *engine* change. Its sink derives the
  header from the record type's serde field names, and a dynamic caller's
  record is a positional `Vec<String>` with none — so a Python-written file
  would have had no header row, which legacy did write. Rather than
  reimplement the sink in the binding, `CsvSinkOps::csv_write_with_header`
  landed on the Rust trait: the same sink with columns supplied explicitly, the
  escape hatch any dynamic caller needs. The read side opens the file once at
  wiring to learn its header, so a replayed row zips back into a dict in
  *column order* (legacy's `HashMap` record lost it).

  **zmq** closed the mechanical tier and extended the seam once more:
  `#[pyadapter]` now accepts a **tuple return**, so a source handing back
  `(data, status)` erases element-wise into a Python tuple of `Stream`s — the
  same spelling `#[pygraph]` already had. `zmq_sub`/`zmq_pub` plus their
  etcd-discovery twins (gated on both features) are bound; `ZmqStatus` erases to
  a `"connected"`/`"disconnected"` string, per the string-selector convention.

  **otlp** is the second binding to need an engine change, for the same reason
  csv did: `otlp_push` took a `&'static str` metric name, so a dynamic caller
  had to `Box::leak` it (as legacy's binding did, on every wiring call). The
  OTel SDK's gauge builder actually takes `impl Into<Cow<'static, str>>`, which
  an owned `String` satisfies — the bound was simply tighter than necessary, and
  relaxing it removes the leak with no effect on existing callers.

  **augurs** closed the stream-transform tier: all six analytics ops, each
  yielding the *full* result as a dict (prediction intervals, per-series
  scores, every detected period) where legacy returned only the headline
  number. Its two input shapes — a single `Stream<f64>` for forecast /
  changepoint / seasons, a `Stream<Vec<f64>>` of one value per series for
  outlier / DTW / cluster — are marshaled inside the fns rather than through
  `typed_input`, because adding a `Vec<f64>` edge conversion would make a
  Python `bytes` silently acceptable where a list of floats is meant.

  **Remaining: 6.** Legacy `wingfoil-python` binds 15 adapters, in four tiers:
  - *mechanical* — **done**: kafka, redis, etcd, fluvio, csv, zmq;
  - *dynamic payload* — kdb, fix: postgres-shaped, needing a `PyPgRow`-style
    stand-in plus column marshaling;
  - *handle pyclass* — web (`WebServer`), prometheus (`PrometheusExporter`):
    stateful objects with a lifecycle, **not** a shape `#[pyadapter]` can
    generate (it has no handle receiver), so these are hand-written over the
    same `PyGraph`/`PyStream` seams;
  - *stream transform* — **done**: otlp, augurs. Legacy exposed these as
    `stream.method(…)`; next uses free fns, a deliberate ergonomic deviation
    for uniformity with the plugin story.

  Two cross-cutting decisions carried by the skill: mode/type selectors take
  **strings** with a loud error rather than `#[pyclass]` enums (legacy has
  `AeronMode` / `Iceoryx2ServiceVariant` / `Iceoryx2Mode`), and conversions
  **fail loudly** rather than defaulting (legacy prometheus/otlp
  `str().unwrap_or_default()` turns a failed conversion into an empty string).
  Both are deviations to note per binding. Sequencing note: adapters needing a
  system library at build time (aeron, iceoryx2) must not join the
  `all-adapters` roll-up that `next-python-test.yml` builds without that job
  also gaining the toolchain install.
- **`wingfoil_next::compat` (`Signal<T>`)** stays a *Rust-side* classic-idiom
  ergonomic (free `ticker`/`constant`, `stream.run`/`peek_value`; `tests/
  compat.rs`) — it is **not** the Python-binding path (that is the object-form
  `PyStream` above).
- **Examples**: port all (order_book, breadth_first, run_mode, latency,
  telemetry/tracing, per-adapter) to idiomatic next (fluent or `graph!`),
  keeping classic versions until Phase 7. 🟢 *landed so far*: order_book,
  breadth_first, run_mode, statistics, threading, async, feedback, and the
  runtime-dynamism pair `dynamic` (`dynamic_group`) + `demux` (`demux_it`), and
  `tracing` (the `log` mode — the `logged` debug tap through `env_logger`).
  Remaining: `latency` / `telemetry` (adapter/cross-process); and the `tracing`
  example's other two modes — `tracing` (route events through a
  `tracing-subscriber`) and `instruments` (engine spans around `run`/cycle) —
  which are ⏳ *blocked* on porting next's `tracing` / `instrument-*` engine
  features (the op catalog logs through `log` only, and the engine has no span
  instrumentation yet). Tracked as a Phase-6 follow-up, landing with the
  instrumentation port, not as example work.
- **Benchmarks**: the four-way `tiers` bench 🟢 *landed* — each workload now
  runs a `classic` (legacy interpreted) bar beside next's
  `interpreted`/`compiled`/`nested`, so `next-interpreted ≥ classic-interpreted`
  is directly readable via `cargo bench --bench tiers`. The baseline now **holds
  on all three workloads**: next-interpreted meets or beats classic on
  `dense_chain` (dispatch-bound), `accumulate` (loop-bound), and wide `fanout`
  (every node fires every cycle); compiled/island win decisively across the
  board (compiled fan-out ~25× either interpreter). ✅ *Resolved*: the earlier
  dense-`fanout` gap (next-interpreted ~40% slower) was the sparse dispatch's
  per-node `BinaryHeap` push/pop; replacing it with classic's layer-bucketed
  drain (`dirty_nodes_by_layer`) closed it — fanout interpreted ~2× faster,
  byte-identical results (guarded by the `Sparse`/`FullSweep`/compiled/nested
  differential suite), no regression on the other workloads. Wiring the bench as
  an automated CI gate is deferred — criterion wall-clock thresholds are too
  noisy for the shared CI runners; it stays a run-on-demand scaffold.

## Phase 7 — cutover

- Deprecate classic engine internals (`MutableNode` wiring path), keep the
  facade API.
- Branch-1 codegen has been retired: `wingfoil::codegen::{generate,
  generate_standalone, StaticRuntime}`, topology fingerprints, golden
  files, and `wingfoil-codegen-build-example` are removed. `Kernel`,
  `KernelWaker`, `waker_channel` remain (they are the engine core now).
- Docs: rewrite crate docs + CLAUDE.md for the op pattern; migration guide
  from `#[node]` to `Op`.
- Version: next merges into `wingfoil` as a major bump.

## Testing strategy

- **Parity oracle**: every ported unit asserts against classic behavior —
  same values, same tick times, same error/bound semantics. Where a test
  would drift for a *documented* reason (e.g. none known today), the test
  states the reason inline.
- **Three-engine agreement**: macro-worthy ops get interpreted vs
  `compiled()` vs `nested()` cross-checks (pattern established in
  `macro_parity.rs` / `nested_islands.rs`).
- **Table-driven three-engine parity across the combinator surface**: a single
  table-driven test file with **one micro-graph per macro-supported
  combinator**, each asserting `interpreted == compiled == nested`. This
  targets the biggest drift surface in the codebase — engine-owned
  initialization and evaluation timing across the three emission paths, which
  is where the real divergences sit (fold seeds `init` interpreted but
  `Default` compiled; closure-factory args re-run per compiled cycle). Because
  the same table row exercises the dispatch flags, it also behaviorally guards
  the activation table — a mis-set `callback_activated`/`always` flag that a
  `..base` struct-update would let compile silently now fails a parity row.
  Seed it with the known-divergent cases first (non-default fold init;
  side-effecting closure factory; `delay(0)` and delay first-value seeding).
- **Duration/bound semantics**: pinned by classic-vs-next parity tests
  (see `duration_bound_matches_classic_engine` — the trailing-cycle
  behavior is classic semantics, deliberately preserved).
- CI: `cargo lint` / `cargo lint-all` / `fmt --check` as today; adapters
  keep feature gates.

## Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Engine-owned init / evaluation-timing drift across the three emission paths | silent wrong values — the macro crate's interpreted/compiled/nested paths are the biggest drift surface; op-`cycle` semantics agree but engine-owned *seeding* and *timing* do not (fold init, closure-factory re-eval) | table-driven three-engine parity test (one micro-graph per macro-supported combinator, `interpreted == compiled == nested`); seed with the known divergences; single seed/init field per op so all three paths read one source |
| Interpreted engine slower than classic on sparse graphs | perf parity claim | ✅ **Phase 4.5 dirty-list landed** (`Dispatch::Sparse`, classic's `dirty_nodes_by_layer` model; `FullSweep` retained as oracle) — results byte-identical, work ∝ active nodes; gated deterministically by `sparse_work_is_independent_of_graph_size` (`tests/sparse_graph.rs`), and `benches/tiers.rs` measures next-interpreted ahead of classic on the sparse groups. Qualifier: ⚠️ **wide *active* fan-ins are 1.45–1.86x slower than classic** — next has no n-ary merge node, so an n-way fan-in costs `n-1` nodes against classic's 1. A Phase 6 gate violation the 10-wide `fanout` bench misses; see Phase 4.5 |
| Arena/SoA slot swap forces a second pass over the ported catalog | rework cost — registrations capture the slot type | ✅ **resolved: boundary frozen by type.** `SlotRef<T>` (`interp.rs`) is the sole access path (`slot()`/`new_slot()` return it; ops only `borrow`/`borrow_mut`); the arena is an internal swap of its innards, zero capture sites touched. Macro uses locals; `Stream::__slot` returns `SlotRef` too |
| Burst/replay semantics drift | backtest determinism is the product | Phase 0.3 spike; classic tests as oracle; fallback design named in advance |
| Feedback timing mismatch | correctness of feedback graphs | engine-level edge + classic's 4 feedback tests; fluent-only v1 |
| Fallibility retrofit cost | touches every emitter | do it first (0.1); never retrofit later |
| Dynamic graph expectations | `graph_node` users | dirty-list engine (the mutable-frontier enabler) has landed; ✅ **the mutation feature has landed** (behind `dynamic-graph`): `Runner::run_dynamic` + an `Extension` scope (append / splice / remove / recycle), `Builder::dynamic_group`, and `Builder::demux`. Islands also cover static composition |
| Python API change (next-python supersedes legacy `wingfoil-python`) | existing `import wingfoil` code must migrate — an accepted breaking change, not drift to avoid | new object-form binding at parity before cutover; next-python `test_interop.py` pytest as gate; migration guide + `wingfoil` module-name takeover at cutover |
| Statistics adapter size | schedule risk, not design risk | it's first in Phase 4 precisely to surface state-porting friction early |

## Explicitly out of scope (v1)

- Feedback inside `graph!` / islands (fluent only).
- Runtime graph mutation — the **Phase 4.5** dirty-list enabler has landed;
  the mutation feature itself is still to be built (open blocker-vs-deviation
  decision), not a permanent exclusion.
- Arena value store for the interpreted engine — a deferred **Phase 4.5** perf
  follow-on with the slot boundary frozen so it stays internal; no longer
  indefinitely deferred and no longer a sequencing risk.
- wingfoil-wasm / wingfoil-js changes (protocol-level, engine-agnostic).

### Nice-to-have (post-v1)

- **Emit-by-reference / zero-copy passthrough.** Today an op reads its
  upstreams by reference (`In<'a> = (&'a A,)`, no clone to inspect) but must
  *produce* an owned value into its own slot — a passthrough or a big-value
  forward costs a clone (cheap only if the element is `Rc`/`Arc`; `store_baseline`
  measures the tax). A node that provably forwards its input unchanged could
  *alias* the upstream slot instead of owning a copy. Purely a memory/throughput
  win — semantics unchanged — so it stays off the correctness-first path.

  **Shape (decided): op-declared aliasing on the frozen slot handle, *not* a
  threaded `'cycle` lifetime.** The clean encoding is a per-op fact —
  `Aliases(input_k)` vs `Owns` — that both engines honour identically:
  interpreted redirects the output `SlotRef` (or arena handle) to the upstream
  slot; compiled reuses the upstream local. The materialize/alias boundary falls
  out correctly: structural passthroughs (`filter`/`sample`/`merge`, and
  `fold`'s publish) alias; retainers (`delay`/`buffer`/`window`) and producers
  (`map`) own — `map` is the case `Rc` could never have helped anyway (it's new
  data). **Soundness rides on the single-fire guarantee**: each node writes its
  slot once per cycle before its readers run, so a within-cycle alias can't be
  mutated out from under a reader; the feature would be unsound in a re-fire
  engine. The alias fact must be a property of the *op* (structurally a
  passthrough), never inferred from a user `map` closure the engine can't see
  into — that keeps the two engines in agreement.

  The type-level alternative — a `'cycle` lifetime threaded through `Op::Out`
  so the borrow checker enforces "can't retain a `&'cycle T`" — was considered
  and **set aside**: it collides with the interpreted engine's `Rc<dyn Any>` /
  `Box<dyn Fn>` erasure (neither is `'static`-free), needs GAT-over-HRTB
  gymnastics, and — worst — an op written with a borrowing `Out<'cycle>` may not
  be expressible identically on both engines, cracking the single-source /
  dual-execution invariant. The op-declared form gets ~all the benefit with
  none of that. Rides on the Phase 4.5 arena (the slot-handle boundary is its
  natural home).

## Deferred / post-v1 work (migrated from tracking issues)

The items below were tracked as GitHub issues (#502, #503, #507) and folded back
into this plan (2026-07-26) so all next-port planning lives in one place. Each is
deferred by design, not dropped.

### Compiled-path IO ingestion — busy-poll sources + bursts (was #502, #503)

One theme: letting the `compiled()` / `graph!` path ingest external /
timestamped data, which it excludes today (capability-matrix rows "Busy-poll
ingest (`ALWAYS`)" and "Bursts (never latest-wins)", both ❌ for compiled;
footnotes 2–3). Both work on the interpreted engine now (`wingfoil_next::Burst`,
`poll`) and feed a compiled island through its inputs.

**Busy-poll ingest (`ALWAYS` sources — classic `poll`/`producer`).**
- *Current state:* excluded — `compiled()` runs its own closed monomorphized
  loop with no external wake, and `graph!` forbids IO-edge sources; `poll` lives
  at the fluent/interpreted layer and feeds a compiled island through its inputs.
- *Why it's now more tractable:* after #496, per-op activation is a monomorphic
  const (`__WF_OP__ACTIVATION`), so `ALWAYS` dispatch already folds correctly for
  ops the compiled path drives (scheduling/always custom ops work). Remaining:
  (a) let an IO-edge/source op live in the compiled graph, and (b) a driving loop
  that re-polls each cycle at a realtime cadence.
- *Open questions:* does compiled keep its "no external wake" character
  (busy-spin only, realtime-timer-driven) or gain a wake channel? Interaction
  with `run_mode` (historical replay of a poll source vs realtime busy-spin)?
  Worth the complexity vs. keeping IO at the interpreted boundary + compiled
  islands?

**Bursts (never latest-wins) in compiled.**
- Every value at one instant grouped and delivered atomically in one cycle —
  never latest-wins, never dropped. Excluded from compiled because no burst
  *sources* exist in the macro vocabulary and the burst pattern is about IO
  ingestion (which compiled excludes). Works on the interpreted engine today
  (`Burst`, matching classic `Burst`/`HistoricalValue`).
- *Scope:* a burst-source shape the `graph!` macro can express and the compiled
  path can drive, delivering same-time-grouped values in one cycle (identical to
  interpreted/classic burst semantics — same-time values ride one burst, not
  coalesced, not split by a monotonic bump).

**Coupling & first decision:** these land together or the exclusion stays —
burst sources are the natural payload of a busy-poll/IO ingest edge. The gating
decision for both: does compiled gain a wake channel, or stay busy-spin +
realtime-timer only? The busy-spin answer fits the compiled-perf story. Tracked
as a capability gap in [`deviation-register.md`](./deviation-register.md) §C.

### Engine architecture / orientation doc (was #507)

An evidence-backed `docs/wingfoil-next-architecture.md` orienting a new
contributor/agent to the Op-pattern engine, citing source at `file:line`.
Deliberately a *current-state snapshot*, not a migration guide — so it is
deferred until after the incoming refactor settles (a snapshot written now would
go stale through it). Sections to regenerate against the post-refactor code:
- The `Op` trait + engine-owned state; `Tick::{Value,Silent,Quiet}`; lifecycle
  hooks.
- The three execution tiers: interpreted sparse dirty-list (`Dispatch::Sparse`,
  default) + full-sweep oracle; fully-monomorphized `compiled()`; `nested()`
  islands.
- Fluent API + `compat::Signal` facade.
- Sources/edges (ticker/constant/poll/external/channel/feedback), bursts, the
  shared `Kernel`.
- Adapters + the "adding an op" recipe (post-#496 `#[op]` forwarder mechanism,
  no macro op-table).
- Testing strategy: parity-oracle vs classic + three-engine agreement.

## Sequencing and parallelism

Phases 0–1 are serial (contract work, ~15% of the effort). Phase 2 groups
parallelize once the recipe is proven on one nontrivial node
(suggested: throttle — scheduling + state + macro row). Phase 4 adapters
are fully independent of each other; statistics can start as soon as
Phase 1 lands. The Phase 6 Python binding (`wingfoil-next-python`, the
object form) can be built out early (it only needs the Phase 1 contract) to
de-risk the Python gate. One PR per node group / adapter; every PR carries its
parity tests.
