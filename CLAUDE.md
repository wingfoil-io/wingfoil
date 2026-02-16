# CLAUDE.md

This file provides guidance for Claude Code when working with this codebase.

## Project Overview

Wingfoil is a Rust stream processing library for building directed acyclic graphs (DAGs) of data transformations. It supports both real-time and historical (backtesting) execution modes.

## Repository Structure

```
wingfoil/           # Core Rust library
  src/
    lib.rs          # Public API re-exports
    types.rs        # Core traits: Element, Node, MutableNode, Stream
    graph.rs        # Graph execution engine
    time.rs         # NanoTime (nanoseconds from UNIX epoch)
    nodes/          # 37 node implementations (map, filter, fold, etc.)
    adapters/       # I/O adapters (CSV, sockets, KDB+, iterators)
    channel/        # Inter-node communication (kanal)
    queue/          # Data structures (TimeQueue, HashByRef)
  examples/         # Usage examples (order_book, rfq, async, breadth_first, circuit_breaker, threading)
  benches/          # Criterion benchmarks

wingfoil-python/    # PyO3 Python bindings
  src/
  python/           # Python package
  tests/            # pytest tests
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

# Benchmarks
cargo bench

# Lint
cargo clippy --workspace --all-targets --all-features
cargo fmt --all -- --check
```

## Development Workflow Rules

### Branch Management

- **NEVER edit files directly on the main branch**
- Before starting any work, always:
  1. Switch to main: `git checkout main`
  2. Pull latest changes: `git pull origin main`
  3. Create a new branch from the updated main: `git checkout -b <branch-name>`
- Branch naming convention: use simple descriptive names (e.g., `add-metrics`, `fix-error-handling`)

### Pre-Commit Checklist

Before committing any changes, ALWAYS run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
```

These commands must pass without errors before creating a commit.

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
