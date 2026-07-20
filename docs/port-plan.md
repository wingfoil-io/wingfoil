# Porting wingfoil to the Op pattern

Status: **draft plan** — no porting work has started. The `wingfoil-next`
crate (branch `claude/wingfoil-next-op-prototype`) is a working prototype of
the target pattern: `Op` trait (pure semantics, engine-owned state), an
interpreted engine, a fully monomorphized `compiled()` expansion, compiled
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
generate_standalone}` + fingerprints + the build-example crate) is retired
at the end — `compiled()` and islands supersede it with strictly better
guarantees.

## Capability matrix

What each execution path supports, per wingfoil pattern. Legend: ✅ works ·
🟡 partial · 📅 planned · ❌ not supported **by design** (not a missing
feature — the path's value depends on the constraint).

Classic is the reference the next engine converges toward: the two
interpreted columns aim to *match* it, while compiled/island add new fast
paths that trade generality for speed (the ❌s are by-design, not gaps).

| Pattern / capability | Classic wingfoil | Interpreted (today) | Interpreted + dirty-list (4.5) | Compiled | Island |
|---|:--:|:--:|:--:|:--:|:--:|
| Static DAG (map/filter/fold/sample/merge/join/…) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Shared nodes / fan-out | ✅ | ✅ | ✅ | ✅ | ✅ |
| Split + glitch-free recombine (single-fire) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Delay & self-scheduling (`SCHEDULES`) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Feedback / cycles | ✅ | ✅¹ | ✅ | ❌ | ❌ |
| Busy-poll ingest (`ALWAYS`) | ✅ | ✅ | ✅ | ❌ | ❌ |
| External / channel / async sources (`THREADED`) | ✅ | ✅ | ✅ | ❌ | ❌ |
| Bursts (never latest-wins) | ✅ | ✅ | ✅ | ❌² | ❌² |
| Historical replay | ✅ | ✅ | ✅ | ✅³ | ✅ |
| Realtime | ✅ | ✅ | ✅ | 🟡³ | ✅ |
| Fallible ops / error propagation | ✅ | ✅ | ✅ | ✅ | ✅ |
| Lifecycle start/stop/teardown | ✅ | ✅ | ✅ | 🟡⁴ | 🟡⁴ |
| Observe arbitrary intermediate streams | ✅ | ✅ | ✅ | ❌⁵ | ❌⁵ |
| Runtime-valued config (params/captures from caller) | ✅ | ✅ | ✅ | ❌⁶ | ❌⁶ |
| Mutable per-node state | ✅⁷ | ✅⁷ | ✅⁷ | ✅⁷ | ✅⁷ |
| Re-run (independent repeated runs) | ✅⁸ | ❌⁸ | 📅⁸ | ✅⁹ | ✅⁹ |
| Dynamic graph (runtime add/remove) | ✅ | ❌ | 📅 | ❌ | 🟡¹⁰ |
| Sparse-graph efficiency (work ∝ *active* nodes) | ✅¹¹ | ❌¹² | ✅ | 🟡¹³ | ✅¹⁴ |
| Dense hot-path speed (measured) | 1× | 1× | ~1× | 3–4×¹⁵ | interior 3–4×¹⁵ |

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
  `setup`. next's v1 Runner is single-run (spike 0.4); matching classic's
  re-run needs the per-node reset hook (planned).
⁹ `compiled()` is a plain fn — each call is a fresh independent run.
¹⁰ Island interior is fixed at compile time, but the island *itself* can be
  wired dynamically into the interpreted graph once 4.5 lands.
¹¹ Classic propagates breadth-first through a dirty-list (work ∝ active
  nodes) — though it still carries an `O(N)` per-cycle reset/scan floor the
  4.5 arena rework can also improve on.
¹² `O(N)` topological sweep every cycle — the Phase 4.5 gap.
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
including re-seeding schedules. Deferred until a use case (backtest sweep,
parameter scan) demands it; the hook slots into the existing lifecycle
machinery when it does. This closes the last Phase-0 spike by decision.

**Gate 0:** all four spikes land with classic-parity tests green.

## Phase 1 — contract completion

- Fold spike results into `op.rs` + all three engines + macro.
- Variadic gaps: `Join3` (trimap), n-ary merge, `try_map`/`try_bimap`/
  `try_trimap` (trivial once cycle is fallible — the closure returns
  `Result`, the op `?`s it).
- Multi-output islands via projection nodes (or explicitly re-defer with a
  written rationale).
- Debug labels on nodes (needed by 0.1 error reports; also unlocks GML
  export in Phase 5).

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

So the places to touch when adding an op:

| Op fits `#[op]`? | Interpreted only | + `graph!`/compiled |
|---|---|---|
| Yes (single-input) | `ops.rs` (`impl` + attr) + fluent method | + `OpKind` variant, `info()` row, parse arm |
| No (multi-input, source, …) | `ops.rs` `impl` + hand `Builder` method + fluent method | + the same macro pieces |

The compiled path can never be zero-touch (constraint #1). The fluent method
can only be auto-generated by making it inherent-on-`Stream` (deliberately not
done — kept trait-based for composability). A completeness test that fails when
an `Op` is registered on one side but not the other is the recommended cheap
guard against drift (not yet added).

Inventory (classic `nodes/` → target), grouped by effort:

| Group | Nodes | Notes |
|---|---|---|
| Done in prototype | map, filter, fold, constant, sample, merge (2-ary), delay, tick(er), producer(→poll), consumer(→for_each), try_map, finally, feedback | parity-tested |
| Trivial state/closure | ✅ distinct, difference, limit, map_filter, throttle, inspect, window, buffer, with_time, ticked_at/-elapsed, not (`tests/catalog.rs`); ⬜ print, timed, split/combine/collapse (Burst/tuple structural) | recipe proven; `window`/`buffer` use `Ctx::is_last_cycle` |
| Scheduling | ✅ throttle; ⬜ delay_with_reset, node_flow (node-level delay/filter/limit/throttle) | `SCHEDULES`/time-gated; pattern proven by delay + throttle |
| Multi-input | ✅ bimap (active/passive) + join, trimap + join3; ⬜ try_* variants | passive `bimap` unlocked passive feedback; `trimap` is the 3-ary combine |
| Engine-touching | always (→`ALWAYS`, done), never, finally (needs teardown), callback stream, iterator_stream (replay source; needs 0.3), receiver, channel nodes (→Phase 3), async_io (→Phase 3) | |
| Structural / deferred | demux, dynamic_group, graph_node | multi-output + dynamic-graph decisions below |

**Dynamic graphs** (`graph_node`, `dynamic_group`, the dynamic examples):
islands already cover *static* subgraphs composed procedurally (including in
loops). Runtime graph *mutation* is a separate feature: either
`Runner::extend` on the interpreted engine (design here, implement if the
demand is real) or an explicit out-of-scope ruling for v1. Do not let this
block the catalog — decide, document, move on.

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
- ⬜ Re-implement classic `threading`/`async` examples on next; bounded-buffer
  back-pressure; the `RunParams` are snapshotted at wiring (classic passes
  them at setup) — align if a producer needs run-time bounds.

## Phase 4 — adapters, easiest-first

Order chosen by (pure → request-shaped → streaming → build-painful):

1. **statistics** — pure computation, the largest single chunk, huge test
   suite, zero IO. Best stress test of engine-owned state; do it first.
   🟡 *started*: all three statistics families now have a representative
   port with parity tests (`tests/statistics.rs`) — exponential (`Ewma`,
   PerTick + clock-driven HalfLife), windowed (`RollingSum`, `RollingMean`
   over a ring buffer), and cumulative is expressible via `fold`. Remaining:
   rolling median/var/std/min-max (monotonic-deque / incremental-moment
   variants), time-windowed rolling, weighted moments.
2. **cache**, **common** (WindowFilter) — small, pure.
3. **csv** — replay source + sink; exercises 0.3 historical bursts.
4. **redis, postgres, etcd** — request/response shaped; fallible cycle +
   lifecycle hooks.
5. **zmq, kafka, kdb** — streaming; `poll`/`external` + lifecycle.
6. **fix** — codec-heavy; fallibility with context.
7. **web** (+ wingfoil-wire-types, wingfoil-wasm, wingfoil-js untouched —
   the wire protocol is engine-agnostic), **prometheus, otlp, augurs**.
8. **aeron, iceoryx2, fluvio** last — build-environment pain (CMake/clang);
   their ring-buffer polling is the natural `ALWAYS`-cap shape.

Each adapter: keep its directory CLAUDE.md, port its tests, one PR each.

**Gate 4:** adapter test suites green on next; classic adapter code paths
untouched (still shipping) until Phase 7.

## Phase 4.5 — engine execution model: breadth-first dirty-list parity

**Gap (must close):** the interpreted engine currently sweeps **all** nodes in
wiring (topological) order every cycle, testing each node's dispatch condition.
Classic wingfoil instead propagates **breadth-first from the ticked source
nodes through a dirty-list / layered schedule**, touching only nodes that can
actually fire this cycle. The two are *observably identical* — both are
glitch-free and fire each node exactly once after its upstreams (macro-parity
tests confirm byte-identical output) — but the mechanisms differ, and the
`O(N)`-per-cycle sweep does not match classic's sparse-graph performance: a
large graph where only a handful of nodes tick still pays for a full node scan
each cycle.

**Target:** reproduce classic's execution model in the interpreted engine —
source-driven breadth-first propagation over a dirty-list (or layer-ordered
work set), so per-cycle work is proportional to the nodes that actually fire,
not the graph size. Concretely:

- Assign each node a layer (longest path from a source) at `build()`, or keep
  an explicit ready/dirty set; either way process in an order that preserves
  the existing glitch-free single-fire guarantee (a recombine node fires once,
  after every upstream that fires this cycle).
- Seed each cycle's work set from the kernel's due callbacks
  (`schedules`/`threaded`/`always` sources) and the tick-propagation frontier,
  then expand breadth-first through active downstream edges only.
- Preserve everything already correct: burst delivery, feedback's `+1`
  scheduled edge, `Activation`-driven dispatch (callback-activated / always),
  passive edges (read-not-triggering), and the `is_last_cycle` boundary flush.

**Scope notes:**
- Pure mechanism/performance change — observable results must stay identical,
  so the full existing parity suite (catalog, macro, feedback, channel) is the
  regression gate, plus a new large-sparse-graph benchmark asserting per-cycle
  cost tracks the *active* node count, not `N`.
- The value store is orthogonal but naturally paired: individual
  `Rc<RefCell<T>>` slots → a contiguous arena/SoA (the other prototype
  simplification the interp module doc flags), which the dirty-list rework is
  the right moment to land.
- The **compiled**/island path is unaffected in shape (it already emits
  straight-line per-node dispatch, the static-schedule analogue), but this is
  where branch-1's *region gating* idea (skip whole quiet sub-graphs) becomes
  the compiled counterpart of the dirty-list — worth doing in the same pass.
- Bench gate ties to Phase 6: `next-interpreted ≥ classic-interpreted` on the
  sparse workloads is only achievable **after** this phase; until then next's
  interpreted engine is knowingly slower on large sparse graphs.

**Dynamism rides on this.** A dirty-list engine that already maintains a
mutable frontier of active nodes is the natural home for **runtime graph
mutation** — classic's `graph_node` / `dynamic_group` (add/remove nodes and
sub-graphs mid-run) live on exactly this machinery, which the topological
all-nodes sweep cannot cleanly express. So the dynamic-graph capability
(previously fenced out of v1) folds in here: once the engine propagates from
a mutable ready-set instead of scanning a fixed node vector, appending nodes
+ slots and splicing edges at runtime becomes tractable, with layer/dirty
bookkeeping updated for the affected region. The compiled and island paths
stay static by design (their whole value is a fixed monomorphized schedule);
dynamism is an interpreted-engine capability, matching classic.

Sequencing: independent of the catalog/adapter volume (Phases 2/4) — it can
land any time before the Phase 6 benchmark gate, and should land before
claiming interpreted-engine performance parity with classic. Dynamic-graph
support can be a follow-on increment once the dirty-list core is in.

## Phase 5 — infrastructure

- **Latency**: stamps ride values as today (`Traced` is just a payload);
  `Ctx` gains a wall-clock accessor for `stamp_precise`-style ops.
  `latency_stages` derive unchanged.
- **Graph export**: GML from `Builder` topology + debug labels.
- **`#[node]` retirement**: replaced by `Op` impls.
- **`#[op]` tooling** ✅ **landed**: `#[op(build = name)]` generates the
  interpreted `Builder` method (over `register_op1`) for single-input ops;
  labels derive from `type_name`; the `graph!`/compiled path is table-driven
  (`OpKind::info`). See **Adding an op** under Phase 2. Still open: a
  completeness test guarding against one-sided registration; extending `#[op]`
  coverage to more shapes; optionally generating the fluent method (only clean
  as inherent-on-`Stream`, deliberately deferred to keep it trait-based).

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
- Retire branch-1 codegen: `wingfoil::codegen::{generate,
  generate_standalone, StaticRuntime}`, topology fingerprints, golden
  files, `wingfoil-codegen-build-example`. `Kernel`, `KernelWaker`,
  `waker_channel` remain (they are the engine core now).
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
- **Duration/bound semantics**: pinned by classic-vs-next parity tests
  (see `duration_bound_matches_classic_engine` — the trailing-cycle
  behavior is classic semantics, deliberately preserved).
- CI: `cargo lint` / `cargo lint-all` / `fmt --check` as today; adapters
  keep feature gates.

## Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Interpreted engine slower than classic on sparse graphs | perf parity claim; `O(N)`/cycle sweep vs classic's dirty-list | **Phase 4.5** breadth-first dirty-list rework; sparse-graph benchmark as gate; results already parity-identical, so it's mechanism/perf only |
| Burst/replay semantics drift | backtest determinism is the product | Phase 0.3 spike; classic tests as oracle; fallback design named in advance |
| Feedback timing mismatch | correctness of feedback graphs | engine-level edge + classic's 4 feedback tests; fluent-only v1 |
| Fallibility retrofit cost | touches every emitter | do it first (0.1); never retrofit later |
| Dynamic graph expectations | `graph_node` users | **Phase 4.5** dirty-list engine is the enabler (mutable frontier); islands cover static composition today |
| Python API drift | downstream breakage | facade keeps bindings stable; pytest as gate |
| Statistics adapter size | schedule risk, not design risk | it's first in Phase 4 precisely to surface state-porting friction early |

## Explicitly out of scope (v1)

- Feedback inside `graph!` / islands (fluent only).
- Runtime graph mutation — now targeted at **Phase 4.5** (the dirty-list
  engine is its enabler), not a permanent exclusion.
- Arena value store for the interpreted engine — now folded into **Phase
  4.5** (the dirty-list rework is the right moment to land it), no longer
  indefinitely deferred.
- wingfoil-wasm / wingfoil-js changes (protocol-level, engine-agnostic).

### Nice-to-have (post-v1)

- **Emit-by-reference / zero-copy passthrough.** Today an op reads its
  upstreams by reference (`In<'a> = (&'a A,)`, no clone to inspect) but must
  *produce* an owned value into its own slot — a passthrough or a big-value
  forward costs a clone (cheap only if the element is `Rc`/`Arc`). A future
  optimisation could let a node that provably forwards its input unchanged
  *alias* the upstream slot instead of owning a copy (or, with the Phase 4.5
  arena, hand out a slot handle rather than a value). Purely a memory/throughput
  win — semantics are unchanged — so it stays out of the correctness-first path.

## Sequencing and parallelism

Phases 0–1 are serial (contract work, ~15% of the effort). Phase 2 groups
parallelize once the recipe is proven on one nontrivial node
(suggested: throttle — scheduling + state + macro row). Phase 4 adapters
are fully independent of each other; statistics can start as soon as
Phase 1 lands. Phase 6 facade can be prototyped early (it only needs the
Phase 1 contract) to de-risk the Python gate. One PR per node group /
adapter; every PR carries its parity tests.
