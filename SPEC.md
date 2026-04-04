# Wingfoil Specification

## Table of Contents

1. [Overview](#overview)
2. [Core Architecture](#core-architecture)
3. [Trait Hierarchy](#trait-hierarchy)
4. [Execution Model](#execution-model)
5. [Time Management](#time-management)
6. [Public API](#public-api)
7. [Node Types](#node-types)
8. [I/O Adapters](#io-adapters)
9. [Configuration](#configuration)
10. [Performance](#performance)
11. [Development Guidelines](#development-guidelines)

---

## Overview

**Wingfoil** is a blazingly fast, highly scalable stream processing framework written in Rust, designed for latency-critical use cases such as electronic trading and real-time AI systems.

### Design Goals

- **Ultra-low latency**: <10 nanoseconds per node cycle overhead
- **Simple and obvious**: Define your graph; Wingfoil manages execution
- **Deterministic**: Historical mode for testing and backtesting
- **Multi-language**: Rust crate + Python bindings (PyO3)
- **Extensible**: I/O adapters for various data sources

### Use Cases

- Financial trading systems
- Real-time AI/ML pipelines
- High-frequency data processing
- Complex event processing

---

## Core Architecture

### Directed Acyclic Graph (DAG)

Wingfoil builds directed acyclic graphs where:
- **Nodes** represent computation steps
- **Edges** represent data flow between nodes
- **Breadth-first execution** ensures O(N) per tick regardless of graph depth

### Repository Structure

```
wingfoil/           # Core Rust library
  src/
    lib.rs          # Public API re-exports
    types.rs        # Core traits: Element, Node, MutableNode, Stream
    graph.rs        # Graph execution engine
    time.rs         # NanoTime (nanoseconds from UNIX epoch)
    nodes/          # Node implementations
    adapters/       # I/O adapters
    channel/        # Inter-node communication
    queue/          # Data structures
  examples/         # Usage examples
  benches/          # Benchmarks

wingfoil-python/    # PyO3 Python bindings
```

---

## Trait Hierarchy

```
Element (Debug + Clone + Default + 'static)
    │
    ▼
MutableNode ───────────────────────────────┐
    │                                       │
    │  cycle(&mut self, state)             │
    │  upstreams() → UpStreams             │
    │  setup(), start(), stop(), teardown()│
    │                                       │
    ├───────────────────── Node ───────────┤
    │  cycle(&self, state)                 │
    │                                       │
    └─────────────── Stream<T> ────────────┘
         peek_value() → T
```

### Element

All stream values must implement `Element`:

```rust
pub trait Element: Debug + Clone + Default + 'static {}
```

For large structs, wrap in `Rc<T>` or `Arc<T>` for cheap cloning.

### MutableNode

Base trait for stateful nodes:

```rust
pub trait MutableNode {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool>;
    fn upstreams(&self) -> UpStreams;
    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()>;
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()>;
    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()>;
    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()>;
    fn type_name(&self) -> String;
}
```

### Node

Immutable wrapper using `RefCell<T: MutableNode>`:

```rust
pub trait Node: MutableNode {
    fn cycle(&self, state: &mut GraphState) -> anyhow::Result<bool>;
    fn setup(&self, state: &mut GraphState) -> anyhow::Result<()>;
    fn start(&self, state: &mut GraphState) -> anyhow::Result<()>;
    fn stop(&self, state: &mut GraphState) -> anyhow::Result<()>;
    fn teardown(&self, state: &mut GraphState) -> anyhow::Result<()>;
}
```

### Stream<T>

Extends `Node` with value access:

```rust
pub trait Stream<T>: Node + StreamPeek<T> + AsNode {}

pub trait StreamPeek<T> {
    fn peek_value(&self) -> T;
    fn peek_ref_cell(&self) -> std::cell::Ref<'_, T>;
}
```

---

## Execution Model

### UpStreams

Nodes declare dependencies via `upstreams()`:

```rust
pub struct UpStreams {
    pub active: Vec<Rc<dyn Node>>,  // Triggers downstream when ticks
    pub passive: Vec<Rc<dyn Node>>,  // Read but don't trigger execution
}
```

### Dependency Types

| Type | Behavior |
|------|----------|
| **Active** | Triggers downstream nodes when they tick |
| **Passive** | Read but don't trigger execution (useful for sampling) |

### Lifecycle Hooks

Nodes can implement these methods:

| Hook | When Called |
|------|-------------|
| `setup()` | At wiring time |
| `start()` | Before first cycle |
| `cycle()` | Each tick |
| `stop()` | After last cycle |
| `teardown()` | After stopping |

---

## Time Management

### NanoTime

Nanoseconds since UNIX epoch:

```rust
pub struct NanoTime(u64);

impl NanoTime {
    pub fn now() -> NanoTime;
    pub fn new(nanos: u64) -> NanoTime;
    pub fn from_kdb_timestamp(ts: i64) -> NanoTime;
    pub fn to_kdb_timestamp(&self) -> i64;
    pub fn from(duration: Duration) -> NanoTime;
    pub fn duration(&self) -> Duration;
}
```

### RunMode

```rust
pub enum RunMode {
    RealTime,                    // Uses wall clock time
    HistoricalFrom(NanoTime),     // Replay from timestamp
}
```

### RunFor

```rust
pub enum RunFor {
    Duration(Duration),  // Run for specified duration
    Cycles(u32),        // Run for exactly n cycles
    Forever,           // Run until stopped externally
}
```

---

## Public API

### Factory Functions

```rust
// Periodic tick generator
pub fn ticker(period: Duration) -> Rc<dyn Node>

// Single-value emitter (ticks once on first cycle)
pub fn constant<T: Element>(value: T) -> Rc<dyn Stream<T>>

// Multiple stream merger
pub fn merge<T: Element>(sources: Vec<Rc<dyn Stream<T>>>) -> Rc<dyn Stream<T>>

// Multi-input mapping
pub fn bimap<A, B, OUT>(a: Rc<dyn Stream<A>>, b: Rc<dyn Stream<B>>, f: fn(A, B) -> OUT) -> Rc<dyn Stream<OUT>>
pub fn trimap<A, B, C, OUT>(a: Rc<dyn Stream<A>>, b: Rc<dyn Stream<B>>, c: Rc<dyn Stream<C>>, f: fn(A, B, C) -> OUT) -> Rc<dyn Stream<OUT>>
```

### StreamOperators<T> (Fluent API)

#### Transformations

```rust
// Transform values
.map(|v| f(v))
.try_map(|v| Ok(f(v)))

// Conditional propagation
.filter(condition)
.filter_value(pred)

// Deduplication
.distinct()

// Time shifting
.delay(duration)
.delay_with_reset(duration)
```

#### Aggregations

```rust
// Accumulation
.fold(initial, |acc, v| { ... })
.reduce(|acc, v| { ... })

// Numeric
.sum()
.average()
.count()

// Delta computation
.difference()
```

#### Time-based

```rust
// Rate limiting
.throttle(interval)
.sample(trigger)

// Time-windowed buffering
.window(interval)
.buffer(capacity)
```

#### Debugging

```rust
// Side effects without changing value
.inspect(|v| { ... })

// Print to stdout
.print()

// Logging
.logged("label", log::Level)
```

#### I/O

```rust
// Collect for testing
.collect()

// Time pairing
.with_time()
.timed()
```

---

## Node Types

### Source Nodes
- `ticker` - Periodic tick generator
- `constant` - Single-value emitter
- `producer` - Closure-based production

### Transformation Nodes
- `map`, `try_map` - Value transformation
- `bimap`, `trimap` - Multi-input mapping
- `filter`, `filter_value` - Conditional propagation
- `distinct` - Deduplication
- `delay`, `delay_with_reset` - Time shifting

### Aggregation Nodes
- `fold`, `reduce` - Accumulation
- `sum`, `average`, `count` - Numeric aggregations
- `difference` - Delta computation
- `window`, `buffer` - Time/capacity buffering

### Flow Control
- `throttle`, `sample` - Rate limiting
- `limit` - Cycle limiting
- `demux`, `demux_it` - Fan-out
- `merge` - Fan-in

### Async/IO
- `produce_async`, `consume_async` - Tokio integration
- `channel` - Inter-thread communication

---

## I/O Adapters

### CSV Adapter (`csv` feature)

```rust
pub fn csv_read<T: KdbDeserialize, F>(
    path: &str,
    get_time: F,
    has_headers: bool,
) -> Rc<dyn Stream<T>>
```

### KDB+ Adapter (`kdb` feature)

```rust
pub fn kdb_read<T, F>(
    conn: &KdbConnection,
    slice_duration: Duration,
    query_fn: F,
) -> Rc<dyn Stream<T>>

pub fn kdb_write<T>(
    upstream: Rc<dyn Stream<T>>,
    conn: &KdbConnection,
    table: &str,
) -> Rc<dyn Node>
```

### etcd Adapter (`etcd` feature)

```rust
pub fn etcd_sub(
    conn: EtcdConnection,
    prefix: &str,
) -> Rc<dyn Stream<Burst<EtcdEvent>>>

pub fn etcd_pub(
    upstream: Rc<dyn Stream<Burst<EtcdEntry>>>,
    conn: EtcdConnection,
    lease_ttl: Option<Duration>,
    force: bool,
) -> Rc<dyn Node>
```

### ZeroMQ Adapter (`zmq-beta` feature)

```rust
pub fn zmq_pub(...) -> Rc<dyn Node>
pub fn zmq_sub(...) -> Rc<dyn Stream<Burst<Vec<u8>>>>
```

### iceoryx2 Adapter (`iceoryx2-beta` feature)

Zero-copy inter-process communication (IPC) via shared memory.

**Requirements (Adapter):**
- Must not require a central daemon process for basic operation.
- Must support a daemonless intra-process mode (`Local` variant) for CI/tests and embedded deployments.
- Must keep IPC as the default behavior for production inter-process usage.
- Must integrate into the wingfoil `cycle()` loop (single-threaded polling) for `Spin` mode, or use a background thread for `Threaded` mode.
- Must support efficient waiting in `Threaded` mode via `WaitSet` (or high-frequency yield) to avoid high CPU usage while maintaining low latency.
- Must support Python bindings for cross-process communication between Python nodes.
- Must document deployment constraints (shared memory locations, container sizing, permissions).

**Important Notes:**
- iceoryx2 is decentralized and does not require a central daemon for basic pub/sub.
- Service discovery and shared memory resources are file-backed (typically `/dev/shm` or `/tmp/iceoryx2`).
- Connection management is managed via `update_connections()` calls in `start()` and periodically in `cycle()`.

```rust
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum Iceoryx2ServiceVariant {
    #[default]
    Ipc,
    Local,
}

/// Polling mode for the iceoryx2 subscriber.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum Iceoryx2Mode {
    /// Polls directly inside the graph `cycle()` loop.
    /// Lowest latency, highest CPU usage (on graph thread).
    #[default]
    Spin,
    /// Polls in a dedicated background thread and delivers via channel.
    /// Higher latency (one channel-hop), lower CPU usage (uses 10µs yield).
    Threaded,
    /// Event-driven WaitSet (true blocking).
    /// Requires publisher signal on matching Event service.
    Signaled,
}

/// Configuration options for an iceoryx2 subscriber.
#[derive(Debug, Clone, Default)]
pub struct Iceoryx2SubOpts {
    pub variant: Iceoryx2ServiceVariant,
    pub mode: Iceoryx2Mode,
}

/// A fixed-size byte buffer that implements `ZeroCopySend`.
/// Used for generic data transfer (e.g. in Python bindings).
#[repr(C)]
#[derive(Debug, Clone, Copy, ZeroCopySend)]
pub struct FixedBytes<const N: usize> {
    pub len: usize,
    pub data: [u8; N],
}

/// Subscribe to a service and receive samples as a stream.
/// Defaults to `Ipc` variant and `Spin` mode.
pub fn iceoryx2_sub<T>(service_name: &str) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static;

/// Subscribe with explicit options.
pub fn iceoryx2_sub_opts<T>(
    service_name: &str,
    opts: Iceoryx2SubOpts,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static;

/// Subscribe with an explicit service variant.
/// Defaults to `Spin` mode.
pub fn iceoryx2_sub_with<T>(
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static;

/// Subscribe to a byte-slice service.
pub fn iceoryx2_sub_slice(service_name: &str) -> Rc<dyn Stream<Burst<Vec<u8>>>>;

/// Publish a stream to a service.
/// Calls update_connections() periodically to detect new subscribers.
pub fn iceoryx2_pub<T>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    service_name: &str,
) -> Rc<dyn Node>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static;

/// Publish with an explicit service variant.
pub fn iceoryx2_pub_with<T>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    service_name: &str,
    variant: Iceoryx2ServiceVariant,
) -> Rc<dyn Node>
where
    T: Element + ZeroCopySend + Clone + Copy + Send + 'static;

/// Publish a stream of bytes as variable-sized samples.
pub fn iceoryx2_pub_slice(
    upstream: Rc<dyn Stream<Burst<Vec<u8>>>>,
    service_name: &str,
) -> Rc<dyn Node>;
```

**Payload Requirements:**
- Must implement `ZeroCopySend`
- Must be `#[repr(C)]`
- Must be self-contained (no heap, no pointers)

**Testing Notes:**
- Unit tests can use the `local` service variant (intra-process) without touching shared memory.
- Integration tests (`iceoryx2-integration-test` feature) verify round-trip IPC between multiple processes.
- Integration tests depend on shared memory availability and permissions and may be sensitive to environment (e.g. Docker `/dev/shm` size).

---

## Configuration

### Feature Flags

| Feature | Description |
|---------|-------------|
| `default` | Includes `async` |
| `full` | All features |
| `async` | Tokio integration |
| `csv` | CSV read/write |
| `kdb` | KDB+ adapter |
| `zmq-beta` | ZeroMQ (beta) |
| `etcd` | etcd adapter |
| `iceoryx2-beta` | iceoryx2 zero-copy IPC (beta) |
| `dynamic-graph-beta` | Runtime graph modification |

---

## Performance

### Benchmarks

- **Overhead**: <10 nanoseconds per node cycle
- **Execution**: O(N) per tick regardless of graph topology
- **Memory**: Nodes wrapped in `Rc<RefCell<...>>` for interior mutability

### Best Practices

Use cheaply cloneable types:
- **Small strings**: `arraystring`
- **Small vectors**: `tinyvec`
- **Larger types**: `Rc<T>` (single-threaded) or `Arc<T>` (multi-threaded)

---

## Development Guidelines

### Branch Management

- **NEVER edit files directly on the main branch**
- Before starting work:
  1. Switch to main: `git checkout main`
  2. Pull latest: `git pull origin main`
  3. Create branch: `git checkout -b <branch-name>`

### Pre-Commit Checklist

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
```

### Testing Conventions

Tests use historical mode for determinism:

```rust
stream.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10)).unwrap();
assert_eq!(expected, stream.peek_value());
```

## Known Issues

- **`zmq_separate_threads` test**: Flaky test with race condition in ZMQ adapter. Pre-existing issue, unrelated to iceoryx2 changes.
- **iceoryx2 benchmarks**: Criterion benches can take a long time and may exceed small CI timeouts. IPC benchmarks also depend on shared memory availability/permissions.

---

## References

- [Crates.io](https://crates.io/crates/wingfoil)
- [Docs.rs](https://docs.rs/wingfoil/latest/wingfoil/)
- [GitHub](https://github.com/wingfoil-io/wingfoil)
- [Python Package](https://pypi.org/project/wingfoil/)
