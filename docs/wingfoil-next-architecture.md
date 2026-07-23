# wingfoil-next — Architecture Overview

> **Current-state snapshot, not a migration guide.** This describes the
> `wingfoil-next` engine as it exists on the `next` branch today. The port is
> in progress and the API is still evolving — see `docs/port-plan.md` for the
> plan and status markers. Everything below is cited to source
> (`file:line`); anything partial or deferred is marked as such.

`wingfoil-next` is a from-scratch redesign of wingfoil's core so that **one
definition of node semantics** can be executed by multiple engines —
interpreted *and* fully compiled — without duplicating cycle logic. The two
crates:

- **`wingfoil-next/`** — the engine: the `Op` trait, the interpreted engine,
  the fluent API, sources, and adapters.
- **`wingfoil-next-macros/`** — the `graph!` proc macro (three execution
  paths from one wiring function) and the `#[op]` attribute.

Both build on the classic crate's shared **`Kernel`** (`wingfoil::codegen::Kernel`:
clock, schedule queue, run bounds, waker), which is the fixed point of the
migration and serves both engines.

---

## Start here

Read in this order:

1. **`wingfoil-next/src/lib.rs`** — the crate rationale and module map
   (`lib.rs:1`). The doc comment explains the three `MutableNode` walls this
   design inverts (`lib.rs:5`).
2. **`wingfoil-next/src/op.rs`** — the `Op` trait (`op.rs:200`). This is the
   whole contract; everything else is an engine that runs it.
3. **`wingfoil-next/src/interp.rs`** — the interpreted engine: `Builder`
   (`interp.rs:218`), `Runner` (`interp.rs:1659`), and the dispatch loop
   (`interp.rs:1780`).
4. **`wingfoil-next/src/fluent.rs`** — the chaining API layered on the builder.
5. **`wingfoil-next-macros/src/lib.rs`** — the `graph!` macro doc comment
   (`lib.rs:1`) explains `compiled()` vs `nested()` and the **open op set** —
   the table-free forwarder dispatch (`lib.rs:92`) — in full.

The clearest runnable examples: `wingfoil-next/examples/hello_graph.rs`,
`odds_evens.rs`, `dual_mode.rs`; the parity tests
`wingfoil-next/tests/macro_parity.rs`, `parity_bugs.rs`, and `custom_op.rs`
(user ops through all three engines).

---

## 1. The `Op` trait — node semantics as a pure function

`op.rs:200`. An op is **only semantics**: it owns no storage, holds no
upstream pointers, and touches the engine only through a narrow `Ctx`. Its
functions are *associated functions*, not methods — an `Op` type is a witness
for semantics, never instantiated (`op.rs:188`), so engines monomorphize the
functions directly.

Associated items:

- **`Cfg`** — construction-time config, *including closures* (a `map`'s `F`
  **is** its config). Held by the engine, passed in by `&mut`.
- **`State`** — per-node mutable state, **owned by the engine** (boxed in the
  interpreted engine, a local in a compiled runner). This is the key
  inversion from classic: state lives outside the node, not in `RefCell`
  struct fields.
- **`In<'a>`** — the typed inputs for one cycle, passed in by the engine
  (values by reference, tick flags where needed). Ops never reach upstream.
- **`Out`** — the produced value type.
- **`const ACTIVATION: Activation`** (`op.rs:205`, type at `op.rs:22`) — a
  *static* declaration of how the engine must activate the op beyond a plain
  data tick: `schedules` (self-schedules time callbacks), `threaded` (fed by
  an external thread/waker, realtime only), `always` (busy-poll every cycle,
  realtime only). Because it's `const`, engines specialise on it at compile
  time — this replaces classic's name-based `can_receive_callbacks` allowlist
  with a contract (`op.rs:18`).

**Fallible cycle** (`op.rs:207`): `cycle -> Result<Tick<Out>>`. The two axes
are kept distinct: `Tick::Quiet` is ordinary control flow (hot path), `Err`
aborts the run (cold). `Tick<T>` (`op.rs:85`) has **three** variants —
`Value(T)`, `Silent(T)`, and `Quiet` — replacing classic's `bool` + hidden
value-slot side channel.

**`Tick::Silent` — update the value slot without ticking** (`op.rs:88`). A
`Silent(T)` cycle writes the node's value slot but propagates **no** downstream
tick: passive readers (sample's data leg, join's other input) see the new
value, yet no downstream node is activated. Classic's `delay` needs exactly
this — store the first upstream value so passive readers never see
`T::default()` before the delay elapses. Earlier in this port that behaviour,
plus `delay`'s zero-delay inline emit, lived as an **engine-level** special
case patched into every runner separately — the design's biggest flagged drift
risk. **#496 promoted it into the `Op` contract** as the third `Tick` variant,
so `Delay::cycle` now expresses its full semantics **once** (`ops.rs:1424`
returns `Tick::Silent` for the seed; zero-delay emits inline in the same
`cycle`) and every engine handles `Silent` generically — the interpreted
engine has a `Tick::Silent` arm at each dispatch site (`interp.rs:563`+). What
was duplicated, drift-prone engine code is now one code path (see §6).

**Lifecycle hooks** (all default to `Ok(())`): `start` (`op.rs:217`, sources
schedule their first activation, IO ops open resources), `stop` (`op.rs:226`,
runs even after a cycle aborts), `teardown` (`op.rs:234`, release resources).
Every lifecycle function is fallible.

**`Ctx`** (`op.rs:100`) is deliberately narrow — time + self-scheduling only
(`time`, `start_time`, `is_last_cycle`, `schedule`). That narrowness is what
lets an op run under a *composite* engine: `Ctx::nested` (`op.rs:136`) points
schedules at a private queue instead of the outer kernel (see islands, §2).

---

## 2. The three execution tiers

All three run **the same `Op::cycle` functions** — there is no duplicated
cycle logic anywhere, which is the load-bearing property (`lib.rs:33`). The
capability matrix in `docs/port-plan.md` (search "Capability matrix") is the
authoritative per-tier breakdown; the by-design limits (`❌`) are constraints
the tier's value depends on, not gaps.

### (a) Interpreted engine — `interp.rs` (default, production)

`Builder` wires nodes into parallel `Vec`s indexed by node position;
`Runner::run` drives them via the shared `Kernel`. The engine owns each
node's value slot (`Rc<RefCell<T>>`, `interp.rs:218`), its `Cfg`+`State`, and
the edges. Each node crosses exactly **one** dyn boundary — a `CycleFn`
closure (`interp.rs:126`) capturing the concrete op's slots and calling the
monomorphized `Op::cycle`.

**Dispatch** (`Dispatch` enum, `interp.rs:1640`): two strategies with
**identical** observable results.

- **`Dispatch::Sparse`** (default, `run_cycles_sparse` at `interp.rs:1780`) —
  a sparse dirty-list / mutable frontier matching classic's
  `dirty_nodes_by_layer`. At `build()` (`interp.rs:1580`) each node gets an
  *active-downstream* adjacency list (`active_downs`, `interp.rs:1601`) and the
  frontier `seed_nodes` is precomputed. Each cycle seeds the work set from the
  frontier (`always` ops + kernel-marked callback-activated ops), then
  propagates the tick frontier forward: a node that ticks marks its active
  downstream neighbours dirty. The work set drains in ascending **node index**
  order via an index min-heap (`interp.rs:1794`) — wiring order is a valid
  topological order over *all* edges (active and passive), because the fluent
  API forces a stream to exist before it is referenced (`interp.rs:1596`).
  Per-cycle work is proportional to nodes that actually fire, not graph size
  `N`, and results are byte-identical to the old full sweep.
- **`Dispatch::FullSweep`** (`run_cycles_full_sweep` at `interp.rs:1861`) —
  the original `O(N)`-per-cycle topological sweep, retained as an **executable
  reference oracle** for differential parity and benchmarking. Select with
  `runner.with_dispatch(Dispatch::FullSweep)` (`interp.rs:1770`).

`run` (`interp.rs:1683`) returns the first `start`/`cycle`/`stop`/`teardown`
error with node context, and still runs `stop`+`teardown` cleanup afterwards.
It rejects invalid mode/source combinations via `bail!` (e.g. `external`/`poll`
in historical mode, `interp.rs:1690`).

### (b) `compiled()` — the whole program

Generated by `graph!` (`lib.rs:14`, `lib.rs:43`). A standalone function that
owns its own `Kernel`, keeps every node's state in **local variables**, runs
the whole cycle loop, and returns the declared output values. LLVM sees the
entire graph as one function → cross-node fusion and constant-folding. **By
design** (`lib.rs:49`, `lib.rs:113`) it is a closed box: static topology,
outputs-only (no runner, no observing internals), and **no IO edges, no
feedback/cycles, no live inputs** (`external`/`poll`/`for_each` live at the
fluent layer). Only a **self-contained** graph (sources inside, no stream
params) emits `compiled()`.

### (c) `nested()` — a compiled island

Generated by `graph!` (`lib.rs:17`, `lib.rs:53`). The same graph packed into
a **single node** mounted inside a larger interpreted graph via
`Builder::composite` (`interp.rs:1530`). The outer engine drives it once per
activation (one dyn call at the boundary), but inside, dispatch is the same
monomorphized straight-line code `compiled()` emits. Inner tickers/delays
can't touch the outer kernel, so the island keeps a **private `TimeQueue`**
and forwards only its earliest pending time outward; time stays globally
consistent because it reads the outer clock (`op.rs:136`). Islands are
**static** (the graph *around* an island can be dynamic/IO-driven, but the
island's own topology is fixed). A documented island limitation:
`is_last_cycle` is **not** propagated inward, so a boundary-flush op inside an
island flushes only on window boundaries, not at the outer run's end
(`op.rs:131`). An **input-taking** graph (`fn ema(g, price: &Stream<f64>)`)
emits `nested()` **only** — it needs its inputs fed, so it can never be a
standalone program (`lib.rs:78`).

---

## 3. The fluent API + `compat` facade

**`fluent.rs`** is *wiring-time only* — it adds nothing to execution
(`fluent.rs:20`). Combinators are **extension traits**, not inherent methods,
so the op vocabulary is open:

- `GraphBuilder` (`fluent.rs:46`) — a graph under construction (cheap to
  clone; all clones share one builder). `build()` consumes it once, poisoning
  it against later wiring (`fluent.rs:135`).
- `SourceOps` (`fluent.rs:148`) — source constructors
  (`ticker`/`constant`/`external`/`channel`/`poll`/`feedback`).
- `StreamOps<T>` (`fluent.rs:297`) — the core combinators (`map`/`fold`/
  `filter`/`join`/`delay`/`window`/`for_each`/…). Every method is a one-liner
  over the `Stream::wire` extension primitive (`fluent.rs:266`) — e.g.
  `self.wire(|b, h| b.map(h, f))`.
- Third-party op traits plug in the same way, over `Stream::wire` /
  `GraphBuilder::source` (`fluent.rs:59`). `StatisticsOps` (`stats.rs:18`) is
  the in-tree example — kept *out* of the prelude, brought in explicitly.

`prelude` (`lib.rs:92`) re-exports `GraphBuilder`, `Stream`, `SourceOps`,
`StreamOps`, and `Burst`.

**`compat.rs`** is the classic-idiom facade (Phase 6 proof-of-concept,
`compat.rs:1`). A `Signal<T>` (`compat.rs:38`) wraps a fluent `Stream` plus
the shared graph and a slot for the `Runner`, reproducing the classic
`ticker(d).count()` → `run(...)` → `peek_value()` shape over the new engine.
`run` (`compat.rs:134`) is single-shot (re-run deferred); `peek_value`
(`compat.rs:156`) panics if called before `run`, mirroring the infallible
classic API. Only a representative slice of the ~40-method surface is
implemented — the rest is mechanical from here (`compat.rs:22`).

---

## 4. Sources / edges

Sources are the graph's entry points, one per activation mode:

| Source | Activation | Modes | Notes |
|---|---|---|---|
| `ticker(period)` | `SCHEDULES` | both | anchored to first activation to avoid drift (`ops.rs:30`, `interp.rs:639`) |
| `constant(v)` | `SCHEDULES` | both | ticks once on first cycle (`ops.rs:77`, `interp.rs:672`) |
| `poll(f)` | `ALWAYS` | realtime only | busy-poll, one value/cycle, lossless; kernel never parks — busy-spin loop (`interp.rs:1277`) |
| `external()` | `THREADED` | realtime only | waker-driven; emits `Burst<T>` of all values since last cycle (`interp.rs:271`) |
| `channel()` | `SCHEDULES`+`THREADED` | both | timestamped; realtime (waker) or historical (deterministic replay on graph clock) (`interp.rs:321`) |
| `feedback()` | `SCHEDULES` | both | acyclic pass-through edge; values arrive next cycle (`interp.rs:1465`) |

**Bursts** (`Burst<T>` = `tinyvec::TinyVec<[T;1]>`, re-exported from the
classic crate at `lib.rs:99`). All non-coalescing sources deliver same-instant
values atomically in **one** burst — never latest-wins, never dropped
(`lib.rs:57`).

**Historical vs realtime.** `external`/`poll` are wall-clock (realtime only)
and `bail!` in historical mode (`interp.rs:1689`). `channel` carries
timestamps and runs in both: in historical mode the receiver's `start` hook
block-collects the whole timestamped stream up front, groups same-time values
into bursts, and schedules one delivery per timestamp (`interp.rs:393`). A
**known deviation** from classic is documented inline (`interp.rs:398`):
classic reads incrementally/non-blocking, whereas next's `start` blocks until
the producer closes and holds the whole feed in memory — fine for finite
offline replay, would deadlock a producer that depends on the graph's output.

**Feedback.** `feedback()` returns an acyclic source + a `FeedbackSink`;
`feedback_send` (fluent `stream.feedback(&sink)`, `interp.rs:1495`) forwards
values unchanged while scheduling the paired source node to emit them at
`time + 1`. Note `FeedbackSink` exposes **no public `send`** (`interp.rs:199`):
sending requires scheduling a *different* node, which the narrow `Ctx`
(self-scheduling only) can't express — deferred until a concrete need.

**Channel envelope** (`channel.rs:32`): `Message<T>` =
`Value`/`ValueAt`/`Checkpoint`/`EndOfStream`/`Error`. `ChannelSender`
(`channel.rs:79`) is `Send`+clonable; `send_at` (`channel.rs:119`) stamps an
explicit graph time for deterministic historical replay; `send_error`
propagates an error that aborts the run.

**`async_source.rs`** (`async` feature) wraps the channel layer as
`produce_async` — an async closure yielding timestamped values, replaying
deterministically in historical mode (`async_source.rs:1`). It documents a gap
vs classic: `RunParams` are handed in at wiring time (before `run`), so a
validating passthrough checks the caller-supplied `start_time` against the
run's real one and aborts on mismatch (`async_source.rs:27`).

**Shared `Kernel`.** All sources drive the classic
`wingfoil::codegen::Kernel` (clock / schedule / `KernelWaker` / ready channel).
The waker/ready channel is only created for realtime source graphs
(`interp.rs:1702`); a historical channel is schedule-driven and needs no waker.

---

## 5. Adapters & how an op/adapter is added

> **Reality check vs the task framing.** Today `wingfoil-next/src/adapters/`
> contains **only `lines` and `csv`** (`adapters/mod.rs:17`). `statistics` is
> **not** an adapter — it is the top-level `stats.rs` `StatisticsOps` trait
> (`stats.rs:18`). `cache` and `common` (WindowFilter) are **planned, not yet
> ported** (`docs/port-plan.md` Phase 4, item 2). The port-plan's capability
> notes agree.

- **`lines`** (`adapters/lines.rs:1`) — dependency-free, line-oriented file
  adapter: historical replay source (`replay_lines`), realtime `poll` tail
  (`tail_lines`), and a `LinesSinkOps` file sink. The smallest complete I/O
  edge in both directions.
- **`csv`** (`adapters/csv.rs:1`, `csv` feature) — serde-typed CSV replay
  source (`csv_read`) + `CsvSinkOps` sink. The parsing cousin of `lines`.

Both are built strictly on the **public** Op-pattern API — sources over
`channel`/`poll`, sinks over `for_each` — and stay out of the prelude
(`adapters/mod.rs:5`).

**Adding an op — the one-op forwarder mechanism.** #496 **deleted** the macro's
`OpKind`/`OpInfo` table (there is now no per-op table anywhere in the macro
crate). The compiled / `nested()` path is **zero-touch** for the common shapes.
Adding a single-active-input op (`map`/`distinct`/`ewma` shape) is two steps and
touches the macro crate not at all:

1. Write the `Op` impl in `ops.rs` and annotate it `#[op(build = name)]`
   (`ops.rs:108` for `Map`). The `#[op]` attribute (`lib.rs:967`, expansion in
   `expand_op` at `lib.rs:1219`) does two things: it generates the interpreted
   `Builder::name` wrapper over `Builder::register_op1` (`interp.rs:529`), node
   label derived from `type_name` — the single-active-input scope
   (`In<'a> = (&'a I,)`, `State: Default`, no lifecycle hooks); and it emits,
   next to the impl, the **naming-convention forwarder functions**
   (`__wf_op_<name>_cycle`, `_cycle_owned`, `_start`, `_start_owned`,
   `_seed_state`, `_seed_value`) plus the monomorphic consts
   `__WF_OP_<NAME>_ACTIVATION` and `__WF_OP_<NAME>_PASSIVE` (`lib.rs:1309`+).
2. Add the 3-line fluent method on `StreamOps` (a trait can't be extended from
   scattered sites, so this stays hand-written — `port-plan.md:282`).

There is no third step. `graph!` (`lib.rs:947`) **never names an op type**: at a
call `.name(args)` it emits calls to that op's forwarders and lets **rustc's
inference** resolve the concrete op from the argument types — including the
node's state local, declared type-less as `let mut __state = Default::default()`
and pinned only through the `<Op>::State` projection in the forwarder signature.
The one fact needed *before* monomorphization — whether dispatch needs a
`__dirty` check — comes from `__WF_OP_<NAME>_ACTIVATION`, re-emitted as a const
and constant-folded into (or out of) the dispatch condition after
monomorphization. LLVM then produces the same machine code the old hand-written
table row did (benchmarked at ~1.01×). Built-in catalog ops and user ops take
the **identical** path, so the built-in catalog is the extension surface's own
best test.

The macro knows exactly **two** method names of its own — the topology
combinators `.map_n(N, f)` and `.fan(N, |s| …)`, which create `N` nodes
(inexpressible per-node) and take integer-literal counts so the unrolled DAG
stays static (`lib.rs:103`). Everything else — `count`, `accumulate`,
`ewma_per_tick`, `ewma_half_life` — is now an ordinary `#[op]` op (`Count`,
`Accumulate`, … in `ops.rs`), which also single-sourced desugars that used to be
written twice (once in the fluent layer, once in the macro).

**Beyond single-input** — the residue that used to be table rows is now carried
by `#[op]` flags and one call-site convention:

- **Multi-input, values-only, all-active (the `join` shape)** — zero macro
  edits. At `.my_join(&other, f)` the macro classifies each `&stream` argument
  (a name bound in this graph) as an **active edge** and everything else as
  `Cfg`; inputs arrive as `(receiver, edges…)` in call order. The interpreted
  side wires through the public `register_op2` (`interp.rs:586`).
- **Passive edges** — `#[op(passive = [i, …])]` emits a `__WF_OP_<NAME>_PASSIVE`
  bitmask const folded into the dispatch condition (sample's data edge is
  `passive = [0]`, `ops.rs:1208`).
- **Seeded accumulators** — `#[op(build = fold, no_builder, init_arg)]`
  (`ops.rs:1136`): the call's plain argument is the initial `State`, cloned by
  derive-emitted `_seed_state`/`_seed_value` forwarders into both the state and
  the value slot (classic pre-first-tick parity).

Tick-flag inputs (`delay`'s `(&T, bool)`), sources (`ticker`/`constant`), and
closure-config `start` hooks are the genuinely-exotic remainder: those ops carry
`no_builder` and hand-written forwarders / `Builder` methods, or run interpreted
only (`nested()` islands remain the escape hatch for anything the residue still
excludes). **The rationale is not duplicated here** — the full decision,
benchmark, and residue table live in `docs/macro-extensibility-decision.md`; the
step-by-step recipe is `docs/port-plan.md` "Adding an op" (`port-plan.md:274`).

**Networked-adapter lifecycle gap.** The referenced
`docs/networked-adapter-pattern.md` does **not exist** on this branch. No
networked adapter (redis/postgres/zmq/kafka/…) is ported yet — they are
Phase 4 items 4–5 (`port-plan.md:374`) needing `poll`/`external` + `start`/
`stop`/`teardown` lifecycle hooks. The `Op` lifecycle primitives exist
(`op.rs:217`+) and file adapters exercise sources/sinks, but the
request/response and streaming networked patterns remain future work.

---

## 6. Testing strategy

The `wingfoil-next/tests/` suite encodes three layers of agreement:

1. **Parity oracle vs classic** — ported units assert against classic
   wingfoil's behaviour, checking **both values and tick times**
   (`port-plan.md:518`). See `tests/engine_semantics.rs`, `statistics.rs`,
   `channel.rs`, `produce_async.rs`.
2. **Three-engine agreement** — macro-dispatched ops assert
   **interpreted == compiled == nested** on the same micro-graph
   (`tests/macro_parity.rs:1`, `catalog.rs`, `nested_islands.rs`,
   `island_scheduling.rs`). The combinator catalog sweep is the intended guard
   against drift (`port-plan.md:525`). Since #496 the *same* mechanism carries
   user ops, so `tests/custom_op.rs` runs out-of-crate ops (plain/generic/
   closure config, single- and multi-input) through all three expansions —
   the extension surface is parity-tested from user position.
3. **Sparse == full-sweep** — `Dispatch::Sparse` is diffed against the
   `Dispatch::FullSweep` oracle (`tests/sparse_graph.rs`).

**Where drift risk lives.** Op `cycle` semantics agree by construction (shared
code). #496 shrank the residue further: `delay`'s first-value seeding and
zero-delay inline emit — previously reproduced separately in each engine, the
biggest flagged hazard — moved **into** `Op::cycle` as `Tick::Silent` (§1), so
they are now shared code too. What remains engine-owned is **init seeding** —
fold's accumulator seed cloned into state *and* value slot, and closure-factory
re-evaluation (`port-plan.md:547`). `#[op(init_arg)]`'s derive-emitted
`_seed_state`/`_seed_value` forwarders now single-source fold's seed across all
three paths, but the discipline still matters. `tests/parity_bugs.rs`
(`parity_bugs.rs:1`) pins the known historical divergences (e.g. the fold
value-slot seeding drift where interpreted seeded `init` but compiled/nested
seeded `Default`).

`trybuild` tests (`tests/trybuild.rs`) pin the macro's compile-time rejections
(wiring hidden in non-wiring code, shadowed passthrough bindings, etc.).

---

## 7. Status / roadmap

Authoritative plan: **`docs/port-plan.md`** (may be mid-update). It tracks the
phased port of the classic codebase onto the Op pattern with ✅/🟡/📅/❌
markers and a per-tier **capability matrix**. Do not duplicate it — orient
from it. Key pointers:

- **Strategy** — parallel port with a compat facade, not an in-place rewrite;
  classic ships until cutover as a permanent parity oracle (`port-plan.md:15`).
- **Deferred / out of scope for the prototype** (`lib.rs:71`): variadic-input
  built-in ops (the shipped `merge`/`join` are fixed at two inputs — the
  `graph!` fallback itself classifies *any* number of `&stream` edges (§5), so
  this is a catalog limit, not a macro one), an arena/SoA value store + the
  slot-API freeze (Phase 4.5 — the slot boundary is deliberately frozen so the
  catalog and adapters aren't touched twice, `interp.rs:26`), and dynamic
  (runtime-mutated) graphs (Phase 4.5 dirty-list is the enabler).
- **`Tick::Silent` is resolved, not open.** #496 promoted it into the `Op`
  contract (`op.rs:88`, §1) — `delay` now expresses its full semantics in
  `cycle` and every engine handles `Silent` generically. The port-plan's
  Phase-1 entry still frames it as an open "contract-shape decision"
  (`port-plan.md:251`); that entry now lags the code.

### Things worth flagging (code vs task framing / plan)

- The adapter set is narrower than commonly described: only `lines` + `csv`
  exist; `statistics` is a trait module (`stats.rs`), not an adapter;
  `cache`/`common` are unimplemented. See §5.
- `docs/networked-adapter-pattern.md` is **absent** — no networked-adapter
  pattern doc or ported networked adapter exists yet.
- The interpreted engine's **sparse dirty-list is already the default**
  (`Dispatch::Sparse`, `interp.rs:1640`), with the full sweep demoted to an
  oracle. The port-plan still frames dirty-list dispatch as the future
  "Phase 4.5" work (`port-plan.md:398`) — the mechanism has landed ahead of
  where the plan's prose places it, so that section reads as behind the code.
- The `graph!` op-set is now **open**: #496 deleted the `OpKind`/`OpInfo` table
  and every op — built-in or user — dispatches through the same `#[op]`-emitted
  forwarders (§5). `docs/macro-extensibility-decision.md` is the decision record.
