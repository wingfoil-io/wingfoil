# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project Overview

Wingfoil is a Rust stream processing library for building directed acyclic
graphs (DAGs) of data transformations, supporting both real-time and
historical (backtesting) execution.

The repository root is **Wingfoil**, the Op-pattern engine. It replaced the
original `MutableNode` engine, which lived under `legacy/` until it reached
parity and was deleted at the cutover (`docs/planning/cutover-plan.md`).

## Repository Structure

```
crates/                     # every Cargo crate in the tree
  wingfoil/            # The engine: op.rs, interp.rs, fluent.rs, ops.rs,
                            #   stats.rs, adapters/, channel.rs, async_source.rs,
                            #   introspect.rs, latency.rs, runtime/, examples/,
                            #   tests/, benches/
  wingfoil-derive/     # nitro! / #[op] proc macros
  wingfoil-python/     # PyO3 Python bindings (built with maturin)
  wingfoil-python-derive/
  wingfoil-wire-types/      # Wire-format types shared by the web adapter and
                            #   wingfoil-wasm — survives cutover
  wingfoil-wasm/            # Browser-side WASM codec (excluded from the default
                            #   workspace) — survives cutover

docs/                       # User-facing docs live at the top level:
                            #   wingfoil-architecture.md (read this first),
                            #   adding-an-op.md (the op recipe + touch-point
                            #   table), migration.md (#[node] -> Op),
                            #   python-interop.md, comparison.md
  release-notes/            #   One page per version, newest first
  decisions/                #   One page per ruling, and every one is settled
                            #   and true of main: runtime ownership, source
                            #   lifecycle, macro extensibility
  planning/                 #   Internal planning: cutover-plan.md +
                            #   cutover-runbook.md (the remaining swap),
                            #   deviation-register.md, introspection-plan.md,
                            #   trading-roadmap.md. port-plan.md is a
                            #   *historical record* of the port, not a backlog
                            #   — open work is in issues
    proposals/              #   Designed and argued, not built: Project
                            #   Lightning — wired-graph codegen (#726) — and
                            #   Project Metal — FPGA/HDL backend (#727). The
                            #   tracking issue is the status; see docs/README.md
                            #   "Ruling or record?" for which dir a page goes in

js/                         # TypeScript client for the web adapter — an npm
                            #   package, not a crate (@wingfoil/client)

scripts/                    # Dev helpers (setup-dev.sh, ci-logs.sh, disk.sh,
                            #   bench-report.sh)
```

## Start here

New to the engine? Read
[`docs/wingfoil-architecture.md`](docs/wingfoil-architecture.md)
before your first non-trivial change — the shape of the thing, the one
decision everything else follows from, and the rules that bite. Porting code
off the legacy engine is [`docs/migration.md`](docs/migration.md).

## The design objective that got us here

**Wingfoil had to become a strict superset of the legacy engine — every node,
adapter, run mode, example, benchmark and binding — so that `legacy/` could be
deleted outright.** It did, and it was. The consequence that still binds: a
legacy capability, example or test case is never silently dropped. Where
wingfoil deviates deliberately, the deviation is on record
(`docs/planning/deviation-register.md`), and the capability matrix in
`docs/planning/port-plan.md` is the historical account of the port.

The parity oracle is gone with the tree. Tests that *recorded* what legacy did
survive as ordinary regression tests with their expectations pinned as captured
constants — `crates/wingfoil/tests/engine_semantics.rs` is the pattern. Do not
weaken those constants; they are the last word on the behaviour they describe.

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

> **One deliberate exception, on record:** `latency_e2e` became `trading_e2e`
> — directory *and* both targets (`trading_e2e_ws_server`,
> `trading_e2e_fix_gw`) — because the old name described one of the things
> that example does rather than what it is. Renaming the directory alone would
> have left the command users type still saying `latency`, which is the whole
> thing being corrected. The cost the rule warns about was paid in full: every
> workflow, Dockerfile, Pulumi stack and doc reference moved with it, and so
> did the name*space* the example emits into — Prometheus metrics and job
> names, iceoryx2 services, `#[type_name]` pins, the OTLP service name and the
> Grafana dashboard UID are all `trading_e2e` now. Every consumer of those
> names lives in the same directory and moved in the same commit.
>
> The line that *did* hold is between names a binary **emits** and names that
> identify **deployed state**. The Pulumi project names, the Packer AMI name
> and the SSM parameter path keep `wingfoil-latency-*`, because a Pulumi
> project name is part of stack identity (renaming orphans running stacks) and
> the SSM path has an IAM grant scoped to it. Those move in a deploy window,
> not in a commit. Use that split for any future example rename.

House style differs by group (`core/` uses `## Sentence-case title` then prose,
snippet, output; `adapters/` uses `# Name Adapter Example (wingfoil)` then
`## Prerequisites` / `## Run` / `## Code` / `## Output`). Match the group you are
adding to.

Sample output in a README must be **real** — run the example and paste what it
prints. Several adapter READMEs were written from invented output and had to be
corrected against the actual `println!`s; do not repeat that.

**So an example whose README pins its output must be deterministic** — run it
under `RunMode::HistoricalFrom(..)`, not `RealTime`. The two rules collide
otherwise: a realtime example driven by a worker thread prints a different
interleaving on consecutive runs (values coalesce into bursts or land after the
error that ends the run), so *whatever* you paste is wrong on the next run and
the README rots by design. Where an example exists specifically to show
realtime behaviour, keep the pinned section to the parts that do not vary and
say in prose that the interleaving depends on scheduling — do not paste a
sample that only reproduces sometimes.

Every crate carries a `README.md`, and `crates/README.md` is the crate map —
keep them current when a crate's role changes.

## Key concepts (how wingfoil differs from legacy)

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

Dedup is **scoped to the instant being pushed to**, and must stay that way.
Entries live in a `BTreeMap` keyed by time, so a push compares only against
that time's bucket. A flat scan of every pending entry — which is what this
queue used to do — is `O(pending)` per schedule, and *pending* here is not
small: the scheduler holds one entry per timer at all times, because a ticker
re-arms itself the moment it fires. That put every timer in the graph on the
cost of every other timer's tick (22 ns per schedule at one pending entry,
686 ns at 1024), and `delay` paid the same shape with every value still in
flight. The earliest instant is additionally held *out* of the map and
refilled lazily, so a single-timer graph — and a fast timer among slow ones —
never touches the map at all.

## Branching: everything merges into `main`

`next` was the long-lived integration branch that staged the replacement
engine in one place. It has landed on `main`, so **`main` is the trunk for
this repository.** There is no longer a second integration branch.

- **NEVER edit files directly on `main`.**
- The workflow for any change, anywhere in the tree:
  1. Cut a feature branch **from `main`**:
     `git checkout main && git pull origin main && git checkout -b <branch-name>`.
  2. Do the work, commit, and push the feature branch.
  3. Open a pull request with **base `main`**.
- Branch naming: simple descriptive names (e.g. `add-metrics`,
  `fix-error-handling`).

> **`next` is retired, not renamed, and the branch is gone.** The CI
> `push`/`pull_request` triggers and cache `save-if` guards that still named
> `next` alongside `main` became inert with it, and were stripped on 2026-08-12
> (`rust-test.yml`, `python-test.yml`, `security-audit.yml`).

## Build Commands

```bash
# Build
cargo build
cargo build --release

# Test
cargo test
cargo test -p wingfoil --all-features

# Python tests
cd crates/wingfoil-python && maturin develop && pytest

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

The Aeron adapter requires clang, libuuid, libbsd, and a recent CMake
(**>=3.30** — the vendored `rusteron-media-driver` Aeron sources set that floor
in their own `cmake_minimum_required`, so 3.28 fails the build script outright
with `CMake 3.30 or higher is required`):

```bash
sudo apt update
sudo apt install clang libclang-dev uuid-dev libbsd-dev

# CMake 3.31 (apt version is too old on many distros)
wget https://github.com/Kitware/CMake/releases/download/v3.31.0/cmake-3.31.0-linux-x86_64.sh
sudo ./cmake-3.31.0-linux-x86_64.sh --prefix=/usr/local --skip-license
```

`libbsd-dev` is the one that is easy to miss, because nothing fails until
**link** time and only on an `--all-features` target: the vendored Aeron C
client emits `-lbsd`, so a long, apparently successful build ends with
`rust-lld: error: unable to find library -lbsd`.

### Disk space

Builds here are big enough to exhaust a dev sandbox, so it is worth knowing
where the space goes. `scripts/disk.sh` reports it and reclaims it:

```bash
scripts/disk.sh          # what is using space
scripts/disk.sh light    # drop examples/benches/incremental, keep deps/
scripts/disk.sh deep     # also remove every target/ dir in the tree
scripts/disk.sh auto     # light (then deep) only if headroom is low
```

Reach for `light` mid-session: it keeps `target/*/deps`, so the next build
relinks instead of recompiling 700+ crates.

`auto` is the unattended form, and it is wired up in three places so you should
rarely have to think about this at all:

- **`.claude/settings.json`** runs it after every `Bash` call in a Claude Code
  session, and sets `CARGO_INCREMENTAL=0` for those sessions (see below — worth
  2.6GB, and an agent doing one-shot builds never collects on incremental).
- **`.cargo-husky/hooks/pre-commit` and `pre-push`** run it before their
  `--all-targets` step, so a low-headroom tree reclaims rather than dying
  partway through the link.

It costs one `df` and prints nothing when there is room, so it is cheap enough
to hang off a per-command hook. Thresholds are in GB and tunable:
`WINGFOIL_DISK_MIN_GB` (default 10) is where it runs `light`, and
`WINGFOIL_DISK_FLOOR_GB` (default 4) is where a still-full tree escalates to
`deep`. The 10GB default is sized off what it has to leave room for: a
`--all-targets` dev build lands around 9.2GB.

**This bites hardest in a Claude Code cloud sandbox**, where the writable disk
is a fixed per-session allowance (~30GB) rather than the size `df` reports for
the device. Two `--all-targets` feature sets plus incremental will spend all
of it. On "no space left on device", `light` first —
deletes still succeed while writes fail, and the freed space is immediately
usable, so the session is recoverable without starting a new one.

Three things make the tree large, and they compound:

- **Feature unification makes a `--workspace` build wide.** Cargo unifies
  features across the workspace, so a plain `cargo build --workspace` compiles
  the union of what every member asks for; `cargo lint` and `cargo lint-all`
  differ only by aeron and iceoryx2. The figures below were measured while
  `legacy/` was still in the tree and are therefore worst-case — a root build
  now compiles this tree's own feature selection and nothing more.
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
commit and push rebuilds, so expect commits to take minutes, not seconds — and
both now open with `scripts/disk.sh auto`, because that `--all-targets` step is
the largest single demand this repo makes on a disk.

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
- **`accumulate()` / `collapse_accumulate()` are test instruments, not output
  edges.** They grow a `Vec` by one entry per tick for the whole run and clone
  it on every tick, so they are unbounded in realtime and `O(n²)` copying in a
  backtest. Use them where a *bounded* run's whole sequence is needed in one
  value afterwards — asserting values and tick times, or tying two runs out
  against each other. Everywhere else — examples included — emit as the run
  progresses: `print()`, `logged(..)`, `for_each(..)` / `for_each_mut(..)`,
  `inspect(..)`, or `window(..)` / `buffer(..)` for a bounded look-back. An
  example that collects into a `Vec` only to `println!` it after the run is the
  anti-pattern; it also stops the same wiring being pointed at a live feed.
- Temp files in tests get unique names (pid + counter) so parallel tests
  never collide; see `crates/wingfoil/tests/lines_adapter.rs`.
- Feature-gated tests start with `#![cfg(feature = "...")]` at file level.
- Adapter and stats ops stay **out of the prelude** — users opt in with
  `use wingfoil::adapters::<name>::...;`.
- No locks on the graph execution path (`cycle` / `start` etc.); use the
  channel layer to talk to background threads.

## Skills — and they are living documents

Three skills carry the step-by-step recipes for the kinds of surface you add
to wingfoil. **Use them for their respective tasks:**

- **`/new-op`** (`.claude/commands/new-op.md`) — adding a node/op to
  the catalog (`ops.rs` / `stats.rs`): the `Op` shape, `#[op(build = …)]`, the
  fluent extension-trait method, `nitro!`/compiled coverage, the `#[pyop]` /
  `pyop_fn!` Python bindings, and the parity + completeness tests.
- **`/new-adapter`** (`.claude/commands/new-adapter.md`) — adding an
  I/O adapter under `src/adapters/`: source/sink shapes, feature gating, the
  parity obligation, and the adapter tests.
- **`/bind-adapter`** (`.claude/commands/bind-adapter.md`) — adding
  the **Python bindings** for an adapter that is already ported:
  `#[pyadapter]` shapes, the feature/wheel roll-ups, dynamic payloads, the GIL
  rules, and the three test tiers. `/new-adapter` links here for its
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
cargo test -p wingfoil --all-features
cargo test -p wingfoil-derive   # a separate package: the line above never builds it
```

Two notes on what those last two lines do and don't reach:

- **`-p wingfoil` does not build the derive crate's tests.** `nitro!`/`#[op]`
  reason about the wiring function they are handed, and that reasoning has unit
  tests of its own in `crates/wingfoil-derive/src/lib.rs`. They only run when
  you name the package. (CI ran nothing that named it until `rust-test.yml`
  grew a step for it — issue #835.)
- **Doc fences are already covered here, and that is a property of `cargo
  test`, not of the flags.** `cargo test` runs the `--doc` target alongside the
  unit and integration ones; `--all-features` matters because most of the doc
  fences live in `adapters/`, which is gated off by default — a default-feature
  run collects 12 of them, all-features 35. **CI needs a separate step for
  this** (`cargo test --doc -p wingfoil --all-features` in `rust-test.yml`),
  because the test leg runs `cargo nextest`, which drives libtest harnesses and
  cannot run doctests at all. So do not "simplify" the local checklist to
  `cargo nextest run`: the doc fences would stop being checked before commit.

All must pass without errors before creating a commit.
