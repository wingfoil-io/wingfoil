# CLAUDE.md — Wingfoil Next

Guidance for Claude Code when working under `next/`. The repo-root
`CLAUDE.md` still applies (branch rules, pre-commit checklist, error-handling
policy); this file adds what is specific to the next engine.

## The one design objective that governs everything

**Wingfoil Next must become a strict superset of legacy wingfoil — every
node, adapter, run mode, example, benchmark and binding — so that `next/`
can be swapped in wholesale for the legacy tree.** When porting anything,
the legacy implementation and its tests are the parity oracle: the next twin
must produce identical values and tick times, or document precisely why it
deviates (capability matrix in `docs/port-plan.md`). Never silently drop a
legacy capability, example, or test case.

## Never depend on the `wingfoil` crate from `next/`

The dependency runs **legacy → next**: `wingfoil` depends on `wingfoil-next`
and re-exports the shared runtime core from it. Never add `wingfoil` as a
(non-dev) dependency of anything under `next/`, and never reach for
`wingfoil::` in `next/` source — the cutover *deletes* the legacy crates, so
any such edge would have to be unpicked first.

Shared machinery goes in `crates/wingfoil-next/src/runtime/` (engine time, run
bounds, the time queue, `Burst`, the `Kernel`, the latency data layer), and
`wingfoil` re-exports it at its historical path. The only permitted edge back
is a **dev**-dependency, for parity tests and comparison benches against the
classic engine. See `docs/cutover-plan.md`.

## Layout

- `crates/wingfoil-next` — the engine (`op.rs`, `interp.rs`), the fluent
  layer (`fluent.rs`), the op catalog (`ops.rs`, `stats.rs`), adapters
  (`src/adapters/`), channel/async sources (`channel.rs`, `async_source.rs`),
  the classic-style facade (`compat.rs`), the shared runtime core
  (`runtime/`), plus `examples/`, `tests/`, `benches/`.
- `crates/wingfoil-next-macros` — `nitro!` (one wiring fn → `interpreted()` /
  `compiled()` / `nested()`) and `#[op]` (generates the interpreted
  `Builder` method and the forwarders `nitro!` dispatches through).
- `docs/port-plan.md` — the phase-by-phase roadmap, the capability matrix,
  and "Adding an op". Read it before adding ops or adapters.

## Key concepts (how next differs from legacy)

- **`Op` trait** (`op.rs`): semantics as associated *functions* —
  `cycle(cfg, state, input, ctx)` — never methods on an instantiated object.
  `Cfg` = construction-time config (closures live here), `State` =
  engine-owned mutable state, `In<'a>` = typed inputs passed in per cycle,
  `Out` = produced value. `const ACTIVATION: Activation` declares scheduling
  behaviour statically (`NONE` / `SCHEDULES` / `THREADED` / `ALWAYS`).
- **`Tick<T>`**: `Value` (tick downstream), `Silent` (update the value slot
  without ticking — what `delay` needs), `Quiet` (nothing).
- **Wiring**: `GraphBuilder` + `Stream<T>` with combinators as *extension
  traits* (`SourceOps`, `StreamOps`, `StatisticsOps`, adapter traits). New
  vocabulary is added via the two public primitives `GraphBuilder::source`
  and `Stream::wire` — never by editing `Stream` itself.
- **Sources**: `ticker` / `constant` / `external` (threaded, realtime-only) /
  `channel` (both modes; timestamped historical replay via `send_at`) /
  `poll` (busy-spin, realtime-only) / `feedback`. Burst delivery everywhere —
  same-instant values grouped, never latest-wins, never dropped.
- **Fallibility**: every lifecycle fn returns `anyhow::Result`;
  `sender.send_error(e)` propagates a producer error into the graph and
  aborts the run with context.
- **`nitro!` has no per-op table**: `#[op(build = name)]` on an `Op` impl
  generates both the interpreted builder method and the forwarders that
  compiled/nested emission dispatches through. Built-in and user ops take
  the identical path.

## Branching: all next work merges into `next`, not `main`

Everything under `next/` is built up on the long-lived **`next` branch** to
stage the replacement engine in one place; when it reaches parity we swap it
in for the legacy tree wholesale. Until that cutover, `next` is the
integration branch for all next work — treat it the way you would treat
`main` for legacy work.

So the workflow for any change under `next/` is:

1. Cut a feature branch **from `next`**:
   `git checkout next && git pull origin next && git checkout -b <branch-name>`.
2. Do the work, commit, and push the feature branch.
3. Open a pull request with **base `next`** — never `main`. The PR merges the
   feature branch back into `next`.

`main` only ever receives the eventual next→main cutover/sync PRs; no
day-to-day next work targets `main`.

## Working conventions

- Tests use `RunMode::HistoricalFrom(NanoTime::ZERO)` for determinism, and
  assert exact values *and* tick times (`with_time()` + `accumulate()`).
- Temp files in tests get unique names (pid + counter) so parallel tests
  never collide; see `tests/lines_adapter.rs`.
- Feature-gated tests start with `#![cfg(feature = "...")]` at file level.
- Adapter and stats ops stay **out of the prelude** — users opt in with
  `use wingfoil_next::adapters::<name>::...;`.
- No locks on the graph execution path (`cycle` / `start` etc.); use the
  channel layer to talk to background threads.

## Skills — and they are living documents

Three skills carry the step-by-step recipes for the kinds of surface you add
to next. **Use them for their respective tasks:**

- **`/new-op-next`** (`.claude/commands/new-op-next.md`) — adding a node/op to
  the catalog (`ops.rs` / `stats.rs`): the `Op` shape, `#[op(build = …)]`, the
  fluent extension-trait method, `nitro!`/compiled coverage, the `#[pyop]` /
  `pyop_fn!` Python bindings, and the parity + completeness tests.
- **`/new-adapter-next`** (`.claude/commands/new-adapter-next.md`) — adding an
  I/O adapter under `src/adapters/`: source/sink shapes, feature gating, the
  parity obligation, and the adapter tests.
- **`/bind-adapter-next`** (`.claude/commands/bind-adapter-next.md`) — adding
  the **Python bindings** for an adapter that is already ported:
  `#[pyadapter]` shapes, the feature/wheel roll-ups, dynamic payloads, the GIL
  rules, and the three test tiers. `/new-adapter-next` links here for its
  Python step, so a brand-new adapter runs both.

**All three are living documents — keep them current.** Every time you
onboard a new op, adapter, or binding, *or change an existing one*, check
whether the work surfaced something the matching skill doesn't yet capture — a
recurring pitfall, a new invariant, a CI gate, a pattern worth codifying, a
deviation that should become a rule — and fold it back into that skill
(ideally in the same PR). The adapter skill already grew several of its rules
exactly this way, and the binding skill was extracted out of it once the first
real adapter binding landed. A skill that has drifted from how we actually
build ops/adapters is a bug; treat updating it as part of "done", not an
optional extra.

## Pre-commit checklist (same as repo root)

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features — catches code behind `async`, `csv`, `augurs`
cargo test -p wingfoil-next --all-features
```
