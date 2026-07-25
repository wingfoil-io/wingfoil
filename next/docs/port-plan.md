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
- downstream users (including wingfoil-python) see no breakage until the
  facade is deliberately deprecated;
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
  ingestion, which the compiled path excludes anyway.
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
  deferred 4.5 arena rework can also improve on.
¹² Sparse dirty-list dispatch (classic's `dirty_nodes_by_layer` model) has
  landed as the default: per-cycle work ∝ active nodes, results byte-identical
  to classic and to the old full sweep (retained as `Dispatch::FullSweep`, an
  executable oracle). The arena/SoA value store — the remaining dense-path
  speedup — is a deferred follow-on with the slot boundary frozen (Phase 4.5).
¹³ Straight-line per-node `if cond` checks (cheap, but every node); region
  gating (skip quiet sub-graphs) is the planned compiled counterpart.
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
   generates the interpreted `Builder` method for single-input ops; add the
   fluent method; for `graph!`/compiled support add the `OpKind` variant +
   `info()` row + parse arm (IO-edge ops skip the macro);
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
  `Builder::name` (a thin wrapper over `Builder::register_op1`), for the
  single-active-input shape (`In<'a> = (&'a I,)`, `State: Default`, no lifecycle
  hooks). Node labels come from `type_name::<X>()` (shortened), not hand-written
  strings. Ops that don't fit (multi-input, passive edges, tick-flag inputs,
  sources, custom state seeds, lifecycle hooks) keep a hand-written `Builder`
  method.
- **Compiled / `graph!`** — one `OpKind::info()` row per op (an `OpInfo`:
  op type, dispatch flags, and the `Inputs`/`CfgInit`/`StateInit` shapes) drives
  every emitter. Named fields make a half-filled row a compile error.

So the places to touch when adding an op — **the compiled path is
zero-touch; there is no macro table**:

| Op shape | Interpreted | `graph!`/compiled |
|---|---|---|
| Single-input (`#[op]` scope) | `ops.rs` (`impl` + attr) + fluent method | nothing — `#[op]`'s forwarders cover it |
| Multi-input, values-only, all-active (the `join` shape) | `ops.rs` `impl` (+ attr with `no_builder`) + `register_op2`-based fluent method | nothing — `&stream` args classify as edges |
| Passive edges (`passive = [..]`) / seeded accumulators (`init_arg`) | hand `Builder` method + fluent method (`bimap` / `fold` are the public primitives) | nothing — attribute flags on `#[op]` |

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
| Trivial state/closure | ✅ distinct, difference, limit, map_filter, throttle, inspect, window, buffer, with_time, ticked_at/-elapsed, not, print, timed (`tests/catalog.rs`, `tests/catalog_ops.rs`); ✅ split, combine, collapse (Burst/tuple structural, `tests/catalog_flow.rs`) | recipe proven; `window`/`buffer` use `Ctx::is_last_cycle`; `combine` builds the burst locally (no shared-cell port) |
| Scheduling | ✅ throttle, delay_with_reset, node_flow (node-level delay/filter/limit/throttle, run over the unit-stream path, `tests/catalog_flow.rs`) | `SCHEDULES`/time-gated; pattern proven by delay + throttle |
| Multi-input | ✅ bimap (active/passive) + join, trimap + join3, try_* variants (`tests/catalog_ops.rs`) | passive `bimap` unlocked passive feedback; `trimap` is the 3-ary combine |
| Engine-touching | always (→`ALWAYS`, done), ✅ never (`tests/catalog_flow.rs`), finally (needs teardown, done), callback stream, iterator_stream (replay source; needs 0.3), receiver, channel nodes (→Phase 3), async_io (→Phase 3) | |
| Structural / deferred | demux, dynamic_group, graph_node | multi-output + dynamic-graph decisions below |

**Dynamic graphs** (`graph_node`, `dynamic_group`, the dynamic examples):
islands already cover *static* subgraphs composed procedurally (including in
loops). Runtime graph *mutation* is a separate feature: either
`Runner::extend` on the interpreted engine (design here, implement if the
demand is real) or an explicit out-of-scope ruling for v1. Do not let this
block the catalog — decide, document, move on.

**Engine coverage note:** `never`, `combine`, and `delay_with_reset` land as
interpreted-engine (fluent) ports — like `feedback` — since a zero-input
source, an n-ary fan-in, and a two-tick-flag scheduling op fall outside the
`#[op]` single-input scope that auto-emits the `graph!`/compiled forwarders.
`split`/`collapse` are pure sugar over `map`/`map_filter`, so they reach every
engine for free. Extending `combine`/`delay_with_reset` to `graph!`/compiled is
a follow-up (as it is for `feedback`).

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
  feeds the main graph over the channel layer — next's take on classic
  `producer()`/`mapper()`, which stay out of the fluent vocabulary (the channel
  primitive replaces the sugar) — and runs in both modes (realtime bursts,
  deterministic historical replay). `async` (`examples/async/`, gated on the
  `async` feature) drives the graph from a `produce_async` producer with the
  graph as consumer. Bounded-buffer back-pressure (`produce_async_bounded`) and
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
   helpers (`compute_time_slices`/`compute_validated_time_slices`) stay with
   their `kdb`/`postgres` readers and land in `common` when those adapters port
   (Phase 4 items 4–5), so only the always-compiled `WindowFilter` surface is
   here; (b) `FileCache`'s log messages drop the classic "KDB " prefix (the
   cache is not kdb-specific in next).
3. **csv** — replay source + sink; exercises 0.3 historical bursts.
   ✅ *done*. The `csv` and `lines` adapters share two fluent primitives so the
   source/sink boilerplate lives in one place: `GraphBuilder::replay_results`
   (queue a finite `Result<(value, time)>` sequence onto a `channel` source and
   close it — the decode-error-then-stop shape `csv_read` needs) and
   `StreamOps::for_each_mut` (the `&mut`-writer sink, wrapping the owned resource
   in a `RefCell` once instead of in every sink). **Deviation (B3)**: next's
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
   **Deviations** (capabilities all preserved): (a) the tokio runtime is the
   caller's — `etcd_sub`/`etcd_pub` take a `&tokio::runtime::Handle` (and
   `etcd_sub` a `RunParams`), matching next's `produce_async` convention rather
   than classic's hidden global runtime; (b) the sink connects eagerly at wiring
   and returns `Result`, so a connection error surfaces before the run rather
   than during it (classic connected lazily inside the consumer); (c) the sink
   is the `EtcdSinkOps` trait only (classic's free `etcd_pub` fn folded into the
   trait, per next's sink-as-trait convention). The sink drives writes with
   `Handle::block_on`, so the graph must be driven from a non-async thread.
   - **Historical mode unsupported:** `etcd_sub` is a live, unbounded,
     wall-clock-stamped watch with no historical timeline to replay, and the
     historical channel path would block-collect it up front and deadlock at
     `start`. `etcd_sub` now returns `Result` and **rejects
     `RunMode::HistoricalFrom` at wiring time** with a clear error; run it under
     `RunMode::RealTime`.
   - **Follow-up — migrate `etcd_pub` to `consume_async`:** the async-sink
     primitive `consume_async` (Phase 3, landed) exists to move `etcd_pub`'s
     writes off the graph thread, but the migration is **deferred**:
     `consume_async` surfaces a write error only on a later cycle (or swallows
     it in the teardown flush), whereas `etcd_pub`'s `force: false`
     conditional-write test aborts a single-cycle (`RunFor::Cycles(1)`) run on
     the very write that fails. Until `consume_async` can preserve that
     final-write abort, `etcd_pub` keeps its per-write `Handle::block_on` path.
   ✅ **redis** *(done)*: Pub/Sub (`redis_sub` source + `RedisSinkOps::redis_pub`
   sink) and Streams (`redis_stream_read` snapshot→tail source +
   `RedisStreamSinkOps::redis_stream_write` sink), behind the `redis` feature.
   Both sources ride `produce_async`; both sinks ride the shared **`consume_async`**
   primitive (redis has no per-write conditional to abort synchronously, unlike
   `etcd_pub`, so the off-thread sink fits — writes land in order and flush at
   teardown). Parity port of the classic adapter's tests as
   `tests/redis_integration.rs` (testcontainers, gated on `redis-integration-test`)
   plus no-service tests in `tests/redis_adapter.rs`; classic example ported to
   `examples/redis_adapter.rs`. **Deviations** (capabilities all preserved), the
   same three as etcd: (a) the tokio runtime is the caller's (`&Handle` + a
   `RunParams` for the sources); (b) the sinks connect eagerly at wiring and
   return `Result`, so a connection error surfaces before the run; (c) the sinks
   are the `RedisSinkOps` / `RedisStreamSinkOps` traits only (classic's free
   functions + operator traits folded into one trait each). Two burst-model
   notes: `redis_stream_read`'s snapshot rides one atomic burst (one shared
   timestamp, as etcd) rather than classic's per-entry `NanoTime::now()`; and both
   sources **reject `RunMode::HistoricalFrom` at wiring time** (live, unbounded,
   wall-clock streams — Pub/Sub has no backlog, the stream tail blocks forever —
   with no historical timeline to replay).
5. **zmq, kafka, kdb** — streaming; `poll`/`external` + lifecycle.
   ✅ **zmq** *(done)*: real-time ØMQ pub/sub — a `zmq_sub` source (a background
   OS thread polling the `SUB` socket + monitor, feeding the `channel` layer,
   returning a `(data, status)` pair) and a `ZeroMqPub::zmq_pub` /
   `zmq_pub_on` sink (a `register_op1` op that binds the `PUB` socket lazily on
   the first cycle, buffers around the ZMQ slow-joiner, and sends `EndOfStream`
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
   `Stream<()>` (not `Rc<dyn Node>`), binding/registering/run-mode-checking
   lazily on the first cycle so a historical run still errors with "real-time"
   before touching the registry; (c) the `bincode` wire envelope is next-local,
   so a next publisher interoperates with a next subscriber but is **not**
   wire-compatible with a classic/Python peer — cross-language interop lands with
   the Python bindings (Phase 6), which is also why the classic `zmq-cross-lang`
   tests are not ported. Realtime-only, so the parity tests assert received
   values (consecutive counters, connection status) rather than exact tick times.
6. **fix** — codec-heavy; fallibility with context.
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
8. **aeron, iceoryx2, fluvio** last — build-environment pain (CMake/clang);
   their ring-buffer polling is the natural `ALWAYS`-cap shape.

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
gap against classic (pending the benchmark below as the standing gate).

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

**Measured baseline** (`benches/store_baseline.rs`, added now): on a large
forwarding graph (8 KiB `Vec` payload through 16 `filter` hops), the owned-`Vec`
run is ~5× an `Rc<Vec>` run of the identical graph — i.e. the per-hop clone tax
that slot-aliasing would remove is *large* for big-payload forwarding, so the
arena+aliasing has real teeth there (ratio is machine-dependent; regenerate
before deciding). The same bench's `sparse_dispatch` group is the standing gate
for the dirty-list: `Dispatch::Sparse` runs ~8× faster than the `FullSweep`
oracle on a graph padded with cold nodes, confirming work ∝ active, not `N`.

### Dynamic graphs (runtime add/remove) — enabler landed, feature not built

The sparse dirty-list maintains a mutable frontier of active nodes — the
natural home for classic's `graph_node` / `dynamic_group` (add/remove nodes
and sub-graphs mid-run), which the old all-nodes sweep could not cleanly
express. The **enabler is now in place**; the feature itself (a
`Runner::extend` appending nodes + slots and splicing edges at runtime, with
layer/dirty bookkeeping updated for the affected region) is **not yet built**.
This is the one open *decision*, not just execution: **is runtime mutation a
cutover blocker under the superset objective, or a documented v1 deviation?**
Classic `graph_node` users force the call. The compiled and island paths stay
static by design (their whole value is a fixed monomorphized schedule);
dynamism is an interpreted-engine capability, matching classic.

**Scope notes:**
- The scheduler change was pure mechanism/performance — observable results
  stayed identical, so the full existing parity suite (catalog, macro,
  feedback, channel) plus the `FullSweep` oracle guard it. Still owed: a
  large-sparse-graph benchmark asserting per-cycle cost tracks the *active*
  node count, not `N`, as the standing gate for the perf-parity claim.
- The **compiled**/island path is unaffected in shape (it already emits
  straight-line per-node dispatch, the static-schedule analogue), but this is
  where branch-1's *region gating* idea (skip whole quiet sub-graphs) becomes
  the compiled counterpart of the dirty-list — worth doing alongside the
  arena/perf pass.
- Bench gate ties to Phase 6: `next-interpreted ≥ classic-interpreted` on the
  sparse workloads is now achievable in principle (the mechanism has landed);
  the benchmark confirms it.

## Phase 5 — infrastructure

- **Latency**: stamps ride values as today (`Traced` is just a payload);
  `Ctx` gains a wall-clock accessor for `stamp_precise`-style ops.
  `latency_stages` derive unchanged.
- **Graph export**: GML from `Builder` topology + debug labels.
- **`#[node]` retirement**: replaced by `Op` impls.
- **`#[op]` tooling** ✅ **landed**: `#[op(build = name)]` generates the
  interpreted `Builder` method (over `register_op1`) for single-input ops;
  labels derive from `type_name`; the `graph!`/compiled path is table-driven
  (`OpKind::info`). See **Adding an op** under Phase 2. The completeness test
  guarding against one-sided registration ✅ landed in Phase 1 as a
  compile-guard (`tests/op_completeness.rs`) — see **Adding an op**. Still
  open: extending `#[op]` coverage to more shapes; optionally generating the
  fluent method (only clean as inherent-on-`Stream`, deliberately deferred to
  keep it trait-based).

## Phase 6 — facade, python, examples, benches

- **Facade** 🟡 *started*: `wingfoil_next::compat` proves the thesis — a
  `Signal<T>` wrapping the fluent `Stream` + the shared graph + a runner
  slot gives classic-idiom code (free `ticker`/`constant`, `stream.run(..)`,
  `stream.peek_value()`) running on the new engine. `tests/compat.rs`
  exercises counter / map-filter-accumulate / fold / constant+delay written
  exactly as classic code. Remaining: the rest of the ~40-method
  `StreamOperators`/`NodeOperators` surface (mechanical) and the true
  `Rc<dyn Stream>` object form if binary-compat with classic is required.
- **wingfoil-python**: rewire to the facade — should be near-transparent;
  its pytest suite is the gate.
- **Examples**: port all (order_book, breadth_first, run_mode, latency,
  telemetry/tracing, per-adapter) to idiomatic next (fluent or `graph!`),
  keeping classic versions until Phase 7.
- **Benchmarks**: rerun the four-way tiers bench as a regression gate —
  next-interpreted ≥ classic-interpreted; compiled/island wins hold
  (dispatch ~2×, inline 3–4× on the measured workloads).

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
| Interpreted engine slower than classic on sparse graphs | perf parity claim | ✅ **Phase 4.5 dirty-list landed** (`Dispatch::Sparse`, classic's `dirty_nodes_by_layer` model; `FullSweep` retained as oracle) — results byte-identical, work ∝ active nodes; remaining: the sparse-graph benchmark as the standing gate |
| Arena/SoA slot swap forces a second pass over the ported catalog | rework cost — registrations capture the slot type | ✅ **resolved: boundary frozen by type.** `SlotRef<T>` (`interp.rs`) is the sole access path (`slot()`/`new_slot()` return it; ops only `borrow`/`borrow_mut`); the arena is an internal swap of its innards, zero capture sites touched. Macro uses locals; `Stream::__slot` returns `SlotRef` too |
| Burst/replay semantics drift | backtest determinism is the product | Phase 0.3 spike; classic tests as oracle; fallback design named in advance |
| Feedback timing mismatch | correctness of feedback graphs | engine-level edge + classic's 4 feedback tests; fluent-only v1 |
| Fallibility retrofit cost | touches every emitter | do it first (0.1); never retrofit later |
| Dynamic graph expectations | `graph_node` users | dirty-list engine (the mutable-frontier enabler) has landed; the mutation feature (`Runner::extend`) is **not yet built** — open decision: cutover blocker vs documented v1 deviation. Islands cover static composition today |
| Python API drift | downstream breakage | facade keeps bindings stable; pytest as gate |
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

## Sequencing and parallelism

Phases 0–1 are serial (contract work, ~15% of the effort). Phase 2 groups
parallelize once the recipe is proven on one nontrivial node
(suggested: throttle — scheduling + state + macro row). Phase 4 adapters
are fully independent of each other; statistics can start as soon as
Phase 1 lands. Phase 6 facade can be prototyped early (it only needs the
Phase 1 contract) to de-risk the Python gate. One PR per node group /
adapter; every PR carries its parity tests.
