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
  wingfoil/            # The engine: op.rs, interp.rs, fluent.rs, ops.rs,
                            #   stats.rs, adapters/, channel.rs, async_source.rs,
                            #   signal.rs, runtime/, examples/, tests/, benches/
  wingfoil-derive/     # nitro! / #[op] proc macros
  wingfoil-python/     # PyO3 Python bindings (built with maturin)
  wingfoil-python-macros/
  wingfoil-wire-types/      # Wire-format types shared by the web adapter and
                            #   wingfoil-wasm — survives cutover
  wingfoil-wasm/            # Browser-side WASM codec (excluded from the default
                            #   workspace) — survives cutover

docs/                       # wingfoil-architecture.md (read this first),
                            #   migration.md (#[node] -> Op), port-plan.md
                            #   (the port roadmap), cutover-plan.md,
                            #   deviation-register.md, design decisions

js/                         # TypeScript client for the web adapter — an npm
                            #   package, not a crate (@wingfoil/client).
                            #   Survives cutover

legacy/                     # The legacy MutableNode engine — deleted at cutover
  wingfoil/                 #   Core library, nodes/, adapters/, examples/, benches/
  wingfoil-derive/          #   #[node] proc macro
  wingfoil-python/          #   Legacy PyO3 bindings

scripts/                    # Dev helpers (setup-dev.sh, ci-logs.sh, disk.sh,
                            #   bench-report.sh)
```

## Start here

New to the engine? Read
[`docs/wingfoil-architecture.md`](docs/wingfoil-architecture.md)
before your first non-trivial change — the shape of the thing, the one
decision everything else follows from, and the rules that bite. Porting code
off the legacy engine is [`docs/migration.md`](docs/migration.md).

## The one design objective that governs everything

**Wingfoil Next must become a strict superset of legacy wingfoil — every
node, adapter, run mode, example, benchmark and binding — so that `legacy/`
can be deleted outright.** When porting anything, the legacy implementation
and its tests are the parity oracle: the next twin must produce identical
values and tick times, or document precisely why it deviates (capability
matrix in `docs/port-plan.md`). Never silently drop a legacy capability,
example, or test case.

## Never depend on the `wingfoil` crate from `crates/`

The dependency runs **legacy → next**: `wingfoil` depends on `wingfoil`
and re-exports the shared runtime core from it. Never add `wingfoil` as a
(non-dev) dependency of anything under `crates/`, and never reach for
`wingfoil::` in its source — the cutover *deletes* the legacy crates, so any
such edge would have to be unpicked first.

Shared machinery goes in `crates/wingfoil/src/runtime/` (engine time, run
bounds, the time queue, `Burst`, the `Kernel`, the latency data layer), and
`wingfoil` re-exports it at its historical path. The only permitted edge back
is a **dev**-dependency, for parity tests and comparison benches against the
legacy engine. See `docs/cutover-plan.md`.

The same rule is why `crates/wingfoil-wire-types`, `crates/wingfoil-wasm` and
`js` sit outside `legacy/`: all three survive the cutover, and
`crates/wingfoil` already depends on wire-types. `js/` is top-level rather
than under `crates/` because it is an npm package, not a Cargo crate.

## Examples: every one is a directory with a README

`crates/wingfoil/examples/` is grouped by what an example teaches:

- `core/` — engine concepts (wiring, run modes, `nitro!` tiers, threading,
  dynamism). No external services; runs with plain `cargo run`.
- `adapters/` — one directory per I/O adapter, named after the *adapter*
  (`adapters/csv/`), feature-gated.
- `showcase/` — multi-process, end-to-end latency demonstrations.

The rules, enforced by `scripts/check-example-docs.sh` in CI:

1. Every example lives in its own directory with `main.rs` **and** `README.md`.
2. Every target is declared explicitly in `Cargo.toml` with a `path`
   (`autoexamples = false`), under the `# Examples` section.
3. Every example is linked from its group's `README.md`, and each group is
   linked from `examples/README.md`.

Target **names** are decoupled from directory names and must not change —
`adapters/csv/` holds the target `csv_adapter`, so `cargo run --example
csv_adapter` keeps working. Renaming a target breaks users' muscle memory and
every doc reference; renaming a directory is free.

House style differs by group (`core/` uses `## Sentence-case title` then prose,
snippet, output; `adapters/` uses `# Name Adapter Example (wingfoil)` then
`## Prerequisites` / `## Run` / `## Code` / `## Output`). Match the group you are
adding to.

Sample output in a README must be **real** — run the example and paste what it
prints. Several adapter READMEs were written from invented output and had to be
corrected against the actual `println!`s; do not repeat that.

Every crate carries a `README.md`, and `crates/README.md` is the crate map —
keep them current when a crate's role changes.

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

### Two clocks: engine time and the wall snap — and who owns each

`Kernel` keeps them as separate fields, and conflating them is the mistake to
avoid:

- **`time`** (`Ctx::time()`) — **engine time**, source-driven. Under
  `HistoricalFrom` it is pure logic: `begin_cycle` pops the earliest scheduled
  callback and sets `time = next.max(time + 1)`, consulting no clock at all.
  Under `RealTime` it is `NanoTime::now().max(time + 1)`. **This is the only one
  business logic may use** — it is what makes replay deterministic.
- **`wall_time`** (`Ctx::wall_time()`) — this cycle's **wall-clock snap**, for
  latency stamping and telemetry. A real clock read in *both* run modes, so
  "time spent" means the same in a backtest as live. Never branch business
  logic on it.

**The kernel owns the snap and takes it lazily, on first read.**
`Kernel::wall_time()` snaps `NanoTime::now()` only if this cycle hasn't yet,
caches it in a `Cell`, and returns the same instant to every later reader in
that cycle. `begin_cycle` does not snap — it only *invalidates*
(`wall_time.set(None)`) on each of its paths. So **a cycle in which no op stamps
reads the clock zero times**, which matters because the read is ~24 ns (see the
`nanotime` bench) and almost nothing wires `latency::Stamp`.

Three invariants follow, all easy to break by accident:

1. **Don't snap eagerly in `begin_cycle`** — that is what the `Cell` exists to
   avoid. A constant restored there lands entirely in the per-cycle intercept.
2. **`Ctx::new` must not copy the snap.** `Ctx` is built once per node per
   cycle, so an eager copy forces the kernel's snap every cycle and undoes the
   laziness. It leaves `wall_time: None` and defers through `Sink::Kernel`.
3. **Keep `Ctx::wall_time()` on `&self`** — the `Cell` (rather than a
   `&mut self` accessor) is what allows this.

**The three tiers differ only in who holds the kernel:**

- **`compiled()`** owns one outright — `let mut __k = Kernel::new(..)` as a
  stack local, driving its own `while __k.begin_cycle(..)` loop. Nodes get
  `Ctx::new(&mut __k, i)`, i.e. the identical lazy path. Nothing about time is
  special-cased for this tier.
- **interpreted** — the `Runner` holds the kernel; same `Ctx::new`, same
  laziness.
- **`nested()` (island)** borrows the outer kernel's values once per activation
  (`__ctx.time()` / `__ctx.wall_time()` / `__ctx.start_time()`) and hands every
  inner node a `Ctx::nested(..)` carrying `wall_time: Some(snap)`, with the sink
  swapped to the composite's private `TimeQueue` — only `next_time()` is
  forwarded outward. Time stays globally consistent because the island reads the
  *outer* engine's clock rather than keeping its own.

  **The island is the one place laziness is given up**, necessarily: the
  composite must resolve the snap before running its interior and cannot know
  whether any inner node will look, so it takes exactly one snap per activation
  either way. Don't "fix" that by making it lazy per inner node — the previous
  design called `NanoTime::now()` per inner node per activation (~24 ns each),
  which was the island tier's entire per-node deficit against `compiled()`.

Two documented island consequences, deliberate: an op's `start` observes
`NanoTime::ZERO` for the wall snap (true of flat graphs too — `start` runs
before the first cycle), and `is_last_cycle` / `run_mode` are not propagated
into an island.

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
cargo test --manifest-path crates/wingfoil/Cargo.toml --all-features

# Python tests
cd crates/wingfoil-python && maturin develop && pytest
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

**`legacy/` is a separate workspace.** It left the root one ahead of the
cutover rename — `wingfoil` becomes `wingfoil`, and one workspace cannot
hold two packages of that name (`docs/cutover-plan.md` 5.0). So nothing above
touches it, and `--manifest-path crates/wingfoil/Cargo.toml` / `--manifest-path crates/wingfoil-python/Cargo.toml` no longer resolve from the
root. Use the nested manifest:

```bash
cargo test   --manifest-path legacy/Cargo.toml --workspace
cargo test   --manifest-path legacy/wingfoil/Cargo.toml --features full
cargo lint-legacy    # clippy, default features (alias)
cargo test-legacy    # the whole legacy workspace (alias)
```

Two consequences to keep in mind. Legacy artifacts now build into
**`legacy/target/`**, not the root `target/` — `scripts/disk.sh` already finds
both. And the git hooks only run the root workspace (`cargo clippy
--workspace`, `cargo test --workspace`), so **a change under `legacy/` is not
covered by the pre-commit or pre-push hook** — run the two aliases above by
hand before pushing legacy work. CI still gates it, in the `Lint legacy` and
`Test (wingfoil) & Coverage` jobs.

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
  **This is much less true since `legacy/` left the workspace** — that
  13-feature roll-up is no longer in the root graph at all, so a root build now
  compiles the next tree's own feature selection and nothing more. The figures
  below predate the split and are therefore worst-case; the legacy tree still
  costs all of it, but only when you build `legacy/Cargo.toml`.
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
  never collide; see `crates/wingfoil/tests/lines_adapter.rs`.
- Feature-gated tests start with `#![cfg(feature = "...")]` at file level.
- Adapter and stats ops stay **out of the prelude** — users opt in with
  `use wingfoil::adapters::<name>::...;`.
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
cargo test --manifest-path crates/wingfoil/Cargo.toml --all-features
```

All must pass without errors before creating a commit.
