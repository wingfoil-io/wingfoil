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

### 0.2 Feedback

The only true DAG-breaker. Sketch: an engine-level edge —

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

### 0.3 Bursts & channel messages

Classic's channel envelope (`HistoricalValue` bursts, `Checkpoint`,
`EndOfStream`, error variants) vs next's one-value-per-cycle. Decision to
validate: **keep the envelope as-is**; endpoints become ops
(`External`/`Poll` + waker for realtime; a scheduling replay source for
historical). Same-time burst members collapse per the kernel's monotonic
time bump — assert against classic's
`same_time_burst_does_not_break_monotonic_engine_time` and the async_io
burst tests. If parity fails, fall back to a burst payload
(`Tick<Burst<T>>`-style) — decide here, not later.

### 0.4 Re-run / runner lifecycle

Classic streams can be run and inspected repeatedly; next's `Runner::run`
consumes bounds per call and external/poll graphs support a single run.
Decide: multiple sequential `run` calls on one `Runner` (classic parity) vs
documented single-run. Leaning: support sequential runs for
historical/timer graphs, single-run for external/poll (already asserted).

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

1. identify `Cfg` / `State` / `In<'a>` / `Out` / `CAPS`;
2. move the classic `cycle` body verbatim into the op (same logic, inputs
   passed in instead of read from upstream `Rc`s);
3. builder method → fluent method → macro `OpSpec` row (where the op is
   macro-worthy — IO-edge ops are not);
4. port the classic node's unit tests as parity tests (values **and** tick
   times).

Inventory (classic `nodes/` → target), grouped by effort:

| Group | Nodes | Notes |
|---|---|---|
| Done in prototype | map, filter, fold, constant, sample, merge (2-ary), delay, tick(er), producer(→poll), consumer(→for_each) | parity-tested |
| Trivial state/closure | distinct, difference, limit, buffer, window, map_filter (filter_map), inspect, print, timed, with_time, graph_state (ticked_at/-elapsed), not/split/combine/collapse (in mod.rs) | afternoons each |
| Scheduling | throttle, delay_with_reset, node_flow (node-level delay/filter/limit/throttle) | `SCHEDULES`; pattern proven by delay |
| Multi-input | bimap(→join, done), trimap(→join3), try_* variants | needs Phase 1 variadics + fallibility |
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

- Port `channel/` endpoints (kanal transport stays) onto ops per the 0.3
  decision; checkpoint/EndOfStream semantics preserved.
- `produce_async` (bounded-buffer bursts, tokio bridge) on the waker
  channel.
- Re-implement classic `threading` and `async` examples on next.

## Phase 4 — adapters, easiest-first

Order chosen by (pure → request-shaped → streaming → build-painful):

1. **statistics** — pure computation, the largest single chunk, huge test
   suite, zero IO. Best stress test of engine-owned state; do it first.
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

## Phase 5 — infrastructure

- **Latency**: stamps ride values as today (`Traced` is just a payload);
  `Ctx` gains a wall-clock accessor for `stamp_precise`-style ops.
  `latency_stages` derive unchanged.
- **Graph export**: GML from `Builder` topology + debug labels.
- **`#[node]` retirement**: replaced by `Op` impls. Optional small `#[op]`
  derive if boilerplate grates (not load-bearing; decide late).

## Phase 6 — facade, python, examples, benches

- **Facade**: classic `Rc<dyn Stream<T>>` / `NodeOperators` reimplemented
  over `Builder` + `Handle`. One wrapper type holding
  `(Rc<RefCell<Builder>>, Handle<T>)` — structurally identical to the
  fluent `Stream`, so most of the facade is renames + trait plumbing.
  Classic examples compile unchanged against it.
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
| Burst/replay semantics drift | backtest determinism is the product | Phase 0.3 spike; classic tests as oracle; fallback design named in advance |
| Feedback timing mismatch | correctness of feedback graphs | engine-level edge + classic's 4 feedback tests; fluent-only v1 |
| Fallibility retrofit cost | touches every emitter | do it first (0.1); never retrofit later |
| Dynamic graph expectations | `graph_node` users | explicit decision in Phase 2; islands cover static composition today |
| Python API drift | downstream breakage | facade keeps bindings stable; pytest as gate |
| Statistics adapter size | schedule risk, not design risk | it's first in Phase 4 precisely to surface state-porting friction early |

## Explicitly out of scope (v1)

- Feedback inside `graph!` / islands (fluent only).
- Runtime graph mutation (pending Phase 2 decision).
- Arena value store for the interpreted engine (perf work, orthogonal;
  slots stay `Rc<RefCell<T>>` until benchmarks demand otherwise).
- wingfoil-wasm / wingfoil-js changes (protocol-level, engine-agnostic).

## Sequencing and parallelism

Phases 0–1 are serial (contract work, ~15% of the effort). Phase 2 groups
parallelize once the recipe is proven on one nontrivial node
(suggested: throttle — scheduling + state + macro row). Phase 4 adapters
are fully independent of each other; statistics can start as soon as
Phase 1 lands. Phase 6 facade can be prototyped early (it only needs the
Phase 1 contract) to de-risk the Python gate. One PR per node group /
adapter; every PR carries its parity tests.
