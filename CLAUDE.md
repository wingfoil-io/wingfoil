# CLAUDE.md

This file provides guidance for Claude Code when working with this codebase.

> **Working under `next/`? Read [`next/CLAUDE.md`](next/CLAUDE.md) first.**
> Wingfoil Next is the Op-pattern engine we are building in the `next/`
> folder to stage the replacement for the legacy tree — when it is ready we
> swap it in wholesale. All of that work has its own conventions and, most
> importantly, a **different branching workflow: branches are cut from and
> pull requests merge into the `next` branch, never `main`** (see
> [next/CLAUDE.md](next/CLAUDE.md#branching-all-next-work-merges-into-next-not-main)).
> This root file still applies (error-handling policy, pre-commit checklist,
> etc.); `next/CLAUDE.md` adds and overrides on top of it.

## Project Overview

Wingfoil is a Rust stream processing library for building directed acyclic graphs (DAGs) of data transformations. It supports both real-time and historical (backtesting) execution modes.

## Repository Structure

```
wingfoil/           # Core Rust library
  src/
    lib.rs          # Public API re-exports
    types.rs        # Core traits: Element, Node, MutableNode, Stream
    graph.rs        # Graph execution engine (RunMode, RunFor)
    time.rs         # NanoTime (nanoseconds from UNIX epoch)
    nodes/          # 40+ node implementations (map, filter, fold, delay, feedback, etc.)
    adapters/       # I/O adapters (CSV, ZMQ, Kafka, KDB+, Redis, Postgres, etcd,
                    #   FIX, web, Aeron, iceoryx2, Fluvio, augurs, Prometheus, OTLP)
                    #   — each adapter directory has its own CLAUDE.md
    channel/        # Inter-node communication (kanal)
    queue/          # Data structures (TimeQueue, ValueAt)
  examples/         # Usage examples (order_book, async, breadth_first, dynamic,
                    #   feedback, threading, plus one per adapter)
  benches/          # Criterion benchmarks

next/               # Wingfoil Next — the Op-pattern engine being built to replace
                    #   the legacy tree wholesale (see next/README.md for the
                    #   design objectives, and next/CLAUDE.md when working in it)
  docs/             # port-plan.md (the port roadmap), design reviews/decisions
  crates/
    wingfoil-next/        # Dual-mode (interpreted + compiled) engine, ops, adapters
    wingfoil-next-macros/ # graph! / #[op] proc macros

wingfoil-derive/    # Proc macros (#[node] attribute)
wingfoil-python/    # PyO3 Python bindings (built with maturin)
  src/
  python/           # Python package
  tests/            # pytest tests
wingfoil-wire-types/ # Wire-format types shared by the web adapter and wingfoil-wasm
wingfoil-wasm/      # Browser-side WASM codec (excluded from the default workspace)
wingfoil-js/        # TypeScript client for the web adapter (@wingfoil/client)
scripts/            # Dev helpers (setup-dev.sh, ci-logs.sh)
```

## System Dependencies

### Aeron adapter

The Aeron adapter requires clang, libuuid, and a recent CMake (>=3.20):

```bash
sudo apt update
sudo apt install clang libclang-dev uuid-dev

# CMake 3.31 (apt version is too old on many distros)
wget https://github.com/Kitware/CMake/releases/download/v3.31.0/cmake-3.31.0-linux-x86_64.sh
sudo ./cmake-3.31.0-linux-x86_64.sh --prefix=/usr/local --skip-license
```

## Build Commands

```bash
# Build
cargo build
cargo build --release

# Test
cargo test
cargo test -p wingfoil
cargo test -p wingfoil-python

# Python tests
cd wingfoil-python && maturin develop && pytest

# TypeScript client tests
cd wingfoil-js && pnpm test

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

- **There is no cheap build.** `wingfoil-python` enables 13 features on
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

## Development Workflow Rules

### Branch Management

- **NEVER edit files directly on the main branch**
- Before starting any work, always:
  1. Switch to main: `git checkout main`
  2. Pull latest changes: `git pull origin main`
  3. Create a new branch from the updated main: `git checkout -b <branch-name>`
- Branch naming convention: use simple descriptive names (e.g., `add-metrics`, `fix-error-handling`)
- **Wingfoil Next work targets the `next` branch, not `main`**: branches for
  work under `next/` are cut from `next` (`git checkout next && git pull
  origin next && git checkout -b <branch-name>`) and their pull requests
  merge into `next` — never into `main`. Only the eventual next→main
  cutover/sync PRs target `main`.

### Pre-Commit Checklist

Before committing any changes, ALWAYS run:
```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features — CI runs this and feature-gated code is easy to miss
```

All three must pass without errors before creating a commit.

## Key Architecture Concepts

### Trait Hierarchy

- `MutableNode` - has `cycle(&mut self)` called each tick
- `Node` - immutable wrapper via `RefCell<T: MutableNode>`
- `Stream<T>` - extends `Node` with `peek_value()` to get current value

### Execution Model

- Nodes declare dependencies via `upstreams()` returning `UpStreams { active, passive }`
- **Active** upstreams trigger downstream nodes when they tick
- **Passive** upstreams are read but don't trigger execution
- Graph executes breadth-first from source nodes

### `TimeQueue` deduplicates by design — don't "fix" it

`queue::TimeQueue<T>` (backs the graph scheduler, `feedback`, `delay`, and
`CallBackStream`) **intentionally suppresses duplicate `(value, time)` pairs**:
pushing a `(value, time)` already queued is a no-op. This is a feature, not a
bug — e.g. a node scheduled for the same instant twice, or the same feedback
value sent twice in one cycle, must collapse to a single event. Do not remove
duplicate suppression.

Because dedup only needs equality (not hashing), the bound is `T: PartialEq`,
**not** `Hash + Eq`. That is deliberate: it lets `f64` (and other float payloads,
which implement `PartialEq` but neither `Hash` nor `Eq`) flow through `delay`,
`feedback`, and `CallBackStream`. Keep the bound at `PartialEq`.

### Custom Nodes

Use the `#[node]` attribute macro on `impl MutableNode` to generate `upstreams()` and `StreamPeekRef`:

```rust
#[node(active = [upstream], output = value: OUT)]
impl<IN, OUT: Element> MutableNode for MyStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> { ... }
}
```

- `active = [f1, f2]` — fields that trigger this node when they tick
- `passive = [f3]` — fields read but not triggering
- `output = field: Type` — emits `impl StreamPeekRef<Type>`
- No `active`/`passive` → source node (default `upstreams()` returns `UpStreams::none()`)
- Complex cases (e.g. `Dep<T>`, `Option<Rc<dyn Node>>`) → write `upstreams()` manually in the impl block; use `#[node(output = ...)]` alone to still get `StreamPeekRef`:
  ```rust
  #[node(output = value: OUT)]
  impl<IN1, IN2, OUT: Element> MutableNode for BiMapStream<IN1, IN2, OUT> {
      fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> { ... }
      fn upstreams(&self) -> UpStreams { /* custom Dep<T> logic */ }
  }
  ```
- Requires `use wingfoil::*` (or explicit `use wingfoil::AsUpstreamNodes`) for the generated code to compile

See `wingfoil/examples/dynamic/dynamic-manual/main.rs` for a fully manual custom node example.

### Common Patterns

- All stream values must implement `Element` (= `Debug + Clone + Default + 'static`)
- Nodes are wrapped in `Rc<RefCell<...>>` for interior mutability
- Factory functions return `Rc<dyn Stream<T>>` or `Rc<dyn Node>`
- Fluent API: `ticker(duration).map(f).filter(g).fold(init, h)`

### Error Handling

Production code must not call `.unwrap()`. Replacement priority:

1. **`?`** — preferred. `cycle/setup/start/stop/teardown` all return
   `anyhow::Result`, so propagate via `?` and add `.context("…")` from
   `anyhow::Context` at I/O boundaries (file open, socket connect, codec decode).
2. **`.expect("invariant: WHY")`** — only when a precondition makes the
   `None`/`Err` branch unreachable. The message must explain the invariant
   (e.g. `expect("current_node_index set during cycle")`).
3. **`.unwrap()`** — allowed inside `#[cfg(test)]` modules and doc comments
   showing example usage; otherwise disallowed.

Mutex poisoning is not recovered: `.lock().expect("<name> mutex poisoned")` is
the pattern. The expect documents intent — a poisoned lock means another
thread panicked while holding it, and we propagate that panic deliberately.

### Run Modes

- `RunMode::RealTime` - uses wall clock time
- `RunMode::HistoricalFrom(NanoTime)` - replay from timestamp (for testing/backtesting)
- `RunFor::Duration(d)`, `RunFor::Cycles(n)`, `RunFor::Forever`

## Testing Conventions

Tests use historical mode for determinism:
```rust
stream.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10)).unwrap();
assert_eq!(expected, stream.peek_value());
```
