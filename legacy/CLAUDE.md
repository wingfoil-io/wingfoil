# CLAUDE.md — legacy wingfoil

Guidance for Claude Code when working under `legacy/`.

> **Read the repo-root [`CLAUDE.md`](../CLAUDE.md) first.** It carries the
> shared policy — build commands, system dependencies, disk space,
> error-handling, the pre-commit checklist — and describes Wingfoil Next,
> which now occupies the repository root. This file adds what is specific to
> the legacy engine and **overrides the branching rule**: legacy branches are
> cut from and merge into `main`, not `next` (see [Branch
> Management](#branch-management) below).

The legacy tree is the original `MutableNode` engine. It keeps shipping and
serves as the permanent parity oracle for the port, and it is deleted
wholesale at cutover — see `docs/cutover-plan.md` at the root. Anything shared
between the two engines belongs in `crates/wingfoil-next/src/runtime/`, which
`wingfoil` re-exports at its historical paths; never add a
`crates/wingfoil-next` → `legacy/` dependency.

## Layout

```
legacy/
  wingfoil/           # Core Rust library
    src/
      lib.rs          # Public API re-exports (incl. re-exports of the shared
                      #   runtime core from wingfoil-next: NanoTime, RunMode,
                      #   RunFor, TimeQueue, Burst, Kernel, the latency layer)
      types.rs        # Core traits: Element, Node, MutableNode, Stream
      graph.rs        # Graph execution engine
      nodes/          # 40+ node implementations (map, filter, fold, delay,
                      #   feedback, etc.)
      adapters/       # I/O adapters (CSV, ZMQ, Kafka, KDB+, Redis, Postgres,
                      #   etcd, FIX, web, Aeron, iceoryx2, Fluvio, augurs,
                      #   Prometheus, OTLP) — each has its own CLAUDE.md
      channel/        # Inter-node communication (kanal)
      queue/          # ValueAt (TimeQueue now lives in wingfoil-next's runtime)
    examples/         # order_book, async, breadth_first, dynamic, feedback,
                      #   threading, plus one per adapter
    benches/          # Criterion benchmarks
  wingfoil-derive/    # Proc macros (#[node] attribute)
  wingfoil-python/    # PyO3 Python bindings (built with maturin)
    src/
    python/           # Python package
    tests/            # pytest tests
```

## Development Workflow Rules

### Branch Management

- **NEVER edit files directly on the main branch**
- Before starting any work under `legacy/`, always:
  1. Switch to main: `git checkout main`
  2. Pull latest changes: `git pull origin main`
  3. Create a new branch from the updated main: `git checkout -b <branch-name>`
- Branch naming convention: use simple descriptive names (e.g., `add-metrics`,
  `fix-error-handling`)
- **Work outside `legacy/` targets the `next` branch, not `main`** — see the
  root `CLAUDE.md`. Only the eventual next→main cutover/sync PRs cross between
  them.

### Build and test — this tree is its own workspace

`legacy/` was taken out of the root cargo workspace ahead of the cutover
rename: `wingfoil-next` becomes `wingfoil`, and one workspace cannot hold two
packages of that name (`docs/cutover-plan.md` 5.0). `legacy/Cargo.toml` is the
workspace root, and **`-p wingfoil` no longer resolves from the repo root** —
every command needs the manifest path (run from the repo root):

```bash
cargo test --manifest-path legacy/wingfoil/Cargo.toml
cargo test --manifest-path legacy/Cargo.toml -p wingfoil-python
cargo lint-legacy   # clippy, default features (alias in .cargo/config.toml)
cargo test-legacy   # the whole legacy workspace (alias)
cd legacy/wingfoil-python && maturin develop && pytest
```

Two things that follow, and both bite silently:

- **The git hooks do not cover this tree.** `pre-commit` and `pre-push` run
  `--workspace` against the *root* workspace, which no longer includes
  `legacy/`. Run `cargo lint-legacy` and `cargo test-legacy` by hand before
  pushing legacy work; CI gates it in the `Lint legacy` and
  `Test (wingfoil) & Coverage` jobs.
- **Artifacts build into `legacy/target/`,** a second multi-GB target dir.
  `scripts/disk.sh` finds every `target/` in the tree, so it reports and
  reclaims both.

`legacy/Cargo.toml` copies the root workspace's `rust-version`, `lints` and
shared dependency versions, because an excluded package cannot inherit them.
Keep them identical to the root's — this file is deleted with the tree.

The pre-commit checklist, the `protoc` and Aeron system dependencies, and the
disk-space notes are all in the root `CLAUDE.md` and apply here unchanged.

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

See `legacy/wingfoil/examples/dynamic/dynamic-manual/main.rs` for a fully manual custom node example.

### Common Patterns

- All stream values must implement `Element` (= `Debug + Clone + Default + 'static`)
- Nodes are wrapped in `Rc<RefCell<...>>` for interior mutability
- Factory functions return `Rc<dyn Stream<T>>` or `Rc<dyn Node>`
- Fluent API: `ticker(duration).map(f).filter(g).fold(init, h)`

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
