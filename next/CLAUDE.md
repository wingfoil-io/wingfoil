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

## Layout

- `crates/wingfoil-next` — the engine (`op.rs`, `interp.rs`), the fluent
  layer (`fluent.rs`), the op catalog (`ops.rs`, `stats.rs`), adapters
  (`src/adapters/`), channel/async sources (`channel.rs`, `async_source.rs`),
  the classic-style facade (`compat.rs`), plus `examples/`, `tests/`,
  `benches/`.
- `crates/wingfoil-next-macros` — `graph!` (one wiring fn → `interpreted()` /
  `compiled()` / `nested()`) and `#[op]` (generates the interpreted
  `Builder` method and the forwarders `graph!` dispatches through).
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
- **`graph!` has no per-op table**: `#[op(build = name)]` on an `Op` impl
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
- New adapters: follow the `/new-adapter-next` skill
  (`.claude/commands/new-adapter-next.md`).

## Pre-commit checklist (same as repo root)

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features — catches code behind `async`, `csv`, `augurs`
cargo test -p wingfoil-next --all-features
```
