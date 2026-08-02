# CLAUDE.md

Guidance for Claude Code when working in this repository.

> **Working under `legacy/`? Read [`legacy/CLAUDE.md`](legacy/CLAUDE.md) too.**
> The legacy tree is the original `MutableNode` engine, kept shipping and
> serving as the parity oracle for the port. It has its own trait hierarchy
> and node conventions, and a **different branching workflow: legacy branches
> are cut from and merge into `main`** (see
> [legacy/CLAUDE.md](legacy/CLAUDE.md#branch-management)). This file still
> applies there (error-handling policy, pre-commit checklist, build commands);
> `legacy/CLAUDE.md` adds and overrides on top of it.

## Project Overview

Wingfoil is a Rust stream processing library for building directed acyclic
graphs (DAGs) of data transformations, supporting both real-time and
historical (backtesting) execution.

The repository root is **Wingfoil Next**, the Op-pattern engine being built to
replace the legacy tree wholesale. When it reaches parity we delete `legacy/`
and drop the `next` prefix from the crate names — the layout is already
arranged so that cutover is a deletion, not a re-organisation.

## Repository Structure

```
crates/                     # every Cargo crate in the tree
  wingfoil-next/            # The engine: op.rs, interp.rs, fluent.rs, ops.rs,
                            #   stats.rs, adapters/, channel.rs, async_source.rs,
                            #   compat.rs, runtime/, examples/, tests/, benches/
  wingfoil-next-macros/     # nitro! / #[op] proc macros
  wingfoil-next-python/     # PyO3 Python bindings (built with maturin)
  wingfoil-next-python-macros/
  wingfoil-wire-types/      # Wire-format types shared by the web adapter and
                            #   wingfoil-wasm — survives cutover
  wingfoil-wasm/            # Browser-side WASM codec (excluded from the default
                            #   workspace) — survives cutover

docs/                       # port-plan.md (the port roadmap), cutover-plan.md,
                            #   design reviews and decisions

js/                         # TypeScript client for the web adapter — an npm
                            #   package, not a crate (@wingfoil/client).
                            #   Survives cutover

legacy/                     # The legacy MutableNode engine — deleted at cutover
  wingfoil/                 #   Core library, nodes/, adapters/, examples/, benches/
  wingfoil-derive/          #   #[node] proc macro
  wingfoil-python/          #   Legacy PyO3 bindings

scripts/                    # Dev helpers (setup-dev.sh, ci-logs.sh, disk.sh)
```

## The one design objective that governs everything

**Wingfoil Next must become a strict superset of legacy wingfoil — every
node, adapter, run mode, example, benchmark and binding — so that `legacy/`
can be deleted outright.** When porting anything, the legacy implementation
and its tests are the parity oracle: the next twin must produce identical
values and tick times, or document precisely why it deviates (capability
matrix in `docs/port-plan.md`). Never silently drop a legacy capability,
example, or test case.

## Never depend on the `wingfoil` crate from `crates/`

The dependency runs **legacy → next**: `wingfoil` depends on `wingfoil-next`
and re-exports the shared runtime core from it. Never add `wingfoil` as a
(non-dev) dependency of anything under `crates/`, and never reach for
`wingfoil::` in its source — the cutover *deletes* the legacy crates, so any
such edge would have to be unpicked first.

Shared machinery goes in `crates/wingfoil-next/src/runtime/` (engine time, run
bounds, the time queue, `Burst`, the `Kernel`, the latency data layer), and
`wingfoil` re-exports it at its historical path. The only permitted edge back
is a **dev**-dependency, for parity tests and comparison benches against the
legacy engine. See `docs/cutover-plan.md`.

The same rule is why `crates/wingfoil-wire-types`, `crates/wingfoil-wasm` and
`js` sit outside `legacy/`: all three survive the cutover, and
`crates/wingfoil-next` already depends on wire-types. `js/` is top-level rather
than under `crates/` because it is an npm package, not a Cargo crate.

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

### `TimeQueue` deduplicates by design — don't "fix" it

`runtime::time_queue::TimeQueue<T>` (backs the graph scheduler, `feedback`,
`delay`, and legacy's `CallBackStream`) **intentionally suppresses duplicate
`(value, time)` pairs**: pushing a `(value, time)` already queued is a no-op.
This is a feature, not a bug — e.g. a node scheduled for the same instant
twice, or the same feedback value sent twice in one cycle, must collapse to a
single event. Do not remove duplicate suppression.

Because dedup only needs equality (not hashing), the bound is `T: PartialEq`,
**not** `Hash + Eq`. That is deliberate: it lets `f64` (and other float
payloads, which implement `PartialEq` but neither `Hash` nor `Eq`) flow
through `delay` and `feedback`. Keep the bound at `PartialEq`.

## Branching: next work merges into `next`, not `main`

Everything at the root is built up on the long-lived **`next` branch** to
stage the replacement engine in one place; when it reaches parity we delete
the legacy tree. Until that cutover, `next` is the integration branch for all
next work — treat it the way you would treat `main` for legacy work.

- **NEVER edit files directly on `next` or `main`.**
- The workflow for any change outside `legacy/`:
  1. Cut a feature branch **from `next`**:
     `git checkout next && git pull origin next && git checkout -b <branch-name>`.
  2. Do the work, commit, and push the feature branch.
  3. Open a pull request with **base `next`** — never `main`.
- Branch naming: simple descriptive names (e.g. `add-metrics`,
  `fix-error-handling`).
- Work under `legacy/` is cut from and merges into `main` instead — see
  `legacy/CLAUDE.md`.

`main` only ever receives the eventual next→main cutover/sync PRs; no
day-to-day next work targets `main`.

## Build Commands

```bash
# Build
cargo build
cargo build --release

# Test
cargo test
cargo test -p wingfoil-next --all-features
cargo test -p wingfoil            # legacy
cargo test -p wingfoil-python     # legacy

# Python tests
cd crates/wingfoil-next-python && maturin develop && pytest
cd legacy/wingfoil-python && maturin develop && pytest

# TypeScript client tests
cd js && pnpm test

# Benchmarks
cargo bench

# Lint (these aliases live in .cargo/config.toml and mirror CI exactly)
cargo lint        # default features
cargo lint-all    # all features — catches code behind `fix`, `csv`, `iceoryx2`, etc.
cargo fmt --all -- --check
```

`protoc` is required on the build machine (a transitive dependency builds proto
files). On Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`, or run
`scripts/setup-dev.sh`. Note this is needed for a *plain* `cargo build
--workspace`, not just `lint-all` — see the feature-unification note below.

### Aeron adapter system dependencies

The Aeron adapter requires clang, libuuid, and a recent CMake (>=3.20):

```bash
sudo apt update
sudo apt install clang libclang-dev uuid-dev

# CMake 3.31 (apt version is too old on many distros)
wget https://github.com/Kitware/CMake/releases/download/v3.31.0/cmake-3.31.0-linux-x86_64.sh
sudo ./cmake-3.31.0-linux-x86_64.sh --prefix=/usr/local --skip-license
```

### Disk space

Builds here are big enough to exhaust a dev sandbox, so it is worth knowing
where the space goes. `scripts/disk.sh` reports it and reclaims it:

```bash
scripts/disk.sh          # what is using space
scripts/disk.sh light    # drop examples/benches/incremental, keep deps/
scripts/disk.sh deep     # also remove every target/ dir in the tree
```

Reach for `light` mid-session: it keeps `target/*/deps`, so the next build
relinks instead of recompiling 700+ crates.

Three things make the tree large, and they compound:

- **There is no cheap build.** `legacy/wingfoil-python` enables 13 features on
  `wingfoil`, and cargo unifies features across a `--workspace` build, so plain
  `cargo build --workspace` already compiles nearly the whole `full` tree.
  `cargo lint` and `cargo lint-all` differ only by aeron and iceoryx2.
- **`lint-all` cannot reuse `lint`'s work.** A different feature set means a
  different metadata hash, so the two pre-commit lints build two full artifact
  sets back to back. If space is tight, `scripts/disk.sh light` between them.
- **`--all-targets` links ~69 example and bench binaries**, each statically
  carrying the entire dependency graph.

That last one is why `[profile.dev.package."*"] debug = false` is set in the
root `Cargo.toml`: debuginfo was over half the bulk and got copied into every
one of those binaries. It takes a single example binary from 260MB to 64MB, and
a full `--all-targets` build (79 binaries) measures **9.2GB** with it — against
roughly 17GB without, derived from the same per-artifact ratios. The override
applies to dependencies only — cargo excludes workspace members from
`package."*"` — so backtraces and debugger stepping through wingfoil code are
unaffected; only frames inside third-party crates lose line numbers.

`target/debug/incremental` is another 2.6GB of that 9.2GB. It pays for itself
across edit-rebuild cycles, but `CARGO_INCREMENTAL=0` (what CI sets) reclaims it
outright if you are only doing one-shot builds.

Also note the git hooks installed by `.cargo-husky`: `pre-commit` runs
`cargo fmt --all` and a full `cargo clippy --workspace --all-targets`, and
`pre-push` runs `cargo build` plus `cargo test` across the workspace. Every
commit and push rebuilds, so expect commits to take minutes, not seconds.

**Toolchain gap (clippy):** CI runs clippy on the current **stable** rustc,
which can be *newer* than the toolchain in a dev sandbox. Newer clippy adds
lints (e.g. `collapsible_match`) that the older one doesn't emit, so a local
`cargo lint` can pass while CI fails with `-D warnings`. If CI's clippy step is
red but local is green, reproduce with CI's version explicitly:

```bash
rustup toolchain install <ci-version>   # e.g. 1.97.0 — see the failing log's clippy URL
cargo +<ci-version> clippy --workspace --all-targets -- -D warnings
cargo +<ci-version> clippy --workspace --all-targets --all-features -- -D warnings
```

## Error Handling

Production code must not call `.unwrap()`. Replacement priority:

1. **`?`** — preferred. Every lifecycle function returns `anyhow::Result`, so
   propagate via `?` and add `.context("…")` from `anyhow::Context` at I/O
   boundaries (file open, socket connect, codec decode).
2. **`.expect("invariant: WHY")`** — only when a precondition makes the
   `None`/`Err` branch unreachable. The message must explain the invariant
   (e.g. `expect("current_node_index set during cycle")`).
3. **`.unwrap()`** — allowed inside `#[cfg(test)]` modules and doc comments
   showing example usage; otherwise disallowed.

Mutex poisoning is not recovered: `.lock().expect("<name> mutex poisoned")` is
the pattern. The expect documents intent — a poisoned lock means another
thread panicked while holding it, and we propagate that panic deliberately.

## Working conventions

- Tests use `RunMode::HistoricalFrom(NanoTime::ZERO)` for determinism, and
  assert exact values *and* tick times (`with_time()` + `accumulate()`).
- Temp files in tests get unique names (pid + counter) so parallel tests
  never collide; see `crates/wingfoil-next/tests/lines_adapter.rs`.
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

## Pre-Commit Checklist

Before committing any changes, ALWAYS run:

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features — CI runs this and feature-gated code is easy to miss
cargo test -p wingfoil-next --all-features
```

All must pass without errors before creating a commit.
