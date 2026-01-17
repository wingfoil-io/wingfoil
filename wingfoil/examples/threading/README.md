# Multi-threading Example

This example demonstrates wingfoil's multi-threaded graph execution using `producer()` and `mapper()` to offload CPU-bound work to worker threads.

## When to Use Threading vs Async

| Use Case | Approach | Reason |
|----------|----------|--------|
| CPU-bound processing | `producer()` / `mapper()` | Utilizes multiple cores for parallel computation |
| I/O-bound operations | `produce_async()` / `consume_async()` | Non-blocking I/O without thread overhead |
| Heavy data transformations | `mapper()` | Process on dedicated thread, keep main graph responsive |
| External data sources | `producer()` | Generate values independently on worker thread |

## API Reference

### `producer(func)`

Spawns a sub-graph on a worker thread that produces values for the main graph.

```rust
pub fn producer<T: Element + Send + Hash + Eq>(
    func: impl FnOnce() -> Rc<dyn Stream<T>> + Send + 'static,
) -> Rc<dyn Stream<TinyVec<[T; 1]>>>
```

- **Input**: A closure that builds and returns a `Stream<T>`
- **Output**: `Stream<TinyVec<[T; 1]>>` - values wrapped in TinyVec (burst container)
- The closure runs on a new worker thread
- Thread communication via kanal channels

### `mapper(func)` (method on Stream)

Processes incoming stream values on a worker thread.

```rust
fn mapper<FUNC, OUT>(self: &Rc<Self>, func: FUNC) -> Rc<dyn Stream<TinyVec<[OUT; 1]>>>
where
    T: Element + Send,
    OUT: Element + Send + Hash + Eq,
    FUNC: FnOnce(Rc<dyn Stream<TinyVec<[T; 1]>>>) -> Rc<dyn Stream<OUT>> + Send + 'static;
```

- **Input**: Closure receiving `Rc<dyn Stream<TinyVec<[T; 1]>>>` - source values wrapped in TinyVec
- **Output**: `Stream<TinyVec<[OUT; 1]>>` - transformed values wrapped in TinyVec
- The closure runs on a new worker thread
- Useful for CPU-intensive transformations

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Main Graph Thread                         │
│  ┌──────────┐      ┌──────────┐      ┌──────────┐              │
│  │ Receiver │ ───► │   map()  │ ───► │ finally()│              │
│  │ Stream   │      │          │      │          │              │
│  └────▲─────┘      └──────────┘      └──────────┘              │
│       │                                                          │
│       │ kanal channel                                            │
└───────┼──────────────────────────────────────────────────────────┘
        │
┌───────┴──────────────────────────────────────────────────────────┐
│                       Mapper Worker Thread                        │
│  ┌──────────┐      ┌──────────┐      ┌──────────┐              │
│  │ Receiver │ ───► │   map()  │ ───► │  Sender  │              │
│  │ Stream   │      │   x*10   │      │          │              │
│  └────▲─────┘      └──────────┘      └──────────┘              │
│       │                                                          │
│       │ kanal channel                                            │
└───────┼──────────────────────────────────────────────────────────┘
        │
┌───────┴──────────────────────────────────────────────────────────┐
│                      Producer Worker Thread                       │
│  ┌──────────┐      ┌──────────┐      ┌──────────┐              │
│  │ ticker() │ ───► │ count()  │ ───► │  Sender  │              │
│  │          │      │          │      │          │              │
│  └──────────┘      └──────────┘      └──────────┘              │
└──────────────────────────────────────────────────────────────────┘
```

## Run Modes

Both `producer()` and `mapper()` support:

- **`RunMode::RealTime`**: Uses wall clock, kanal channels notify main graph when data ready
- **`RunMode::HistoricalFrom(NanoTime)`**: Simulated time for deterministic testing/backtesting

## Example Code

```rust
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use tinyvec::TinyVec;
use wingfoil::*;

let period = Duration::from_millis(50);
let run_for = RunFor::Duration(period * 10);

// Producer runs on worker thread, generates 1, 2, 3, ...
let produce_graph = move || {
    ticker(period)
        .count()
        .limit(10)
        .map(|x| {
            println!("[Producer] {} on {:?}", x, thread::current().id());
            x
        })
};

// Mapper runs on another worker thread
// Input is wrapped: TinyVec<[TinyVec<[u64; 1]>; 1]>
let map_graph = move |src: Rc<dyn Stream<TinyVec<[TinyVec<[u64; 1]>; 1]>>>| {
    src.map(|xs| {
        xs.iter().flatten().map(|x| x * 10).collect::<Vec<u64>>()
    })
};

producer(produce_graph)
    .mapper(map_graph)
    .map(|xs| xs.iter().flatten().map(|x| x * 10).collect::<Vec<u64>>())
    .finally(|results, _| {
        let flat: Vec<u64> = results.into_iter().flatten().collect();
        println!("Results: {:?}", flat);  // [100, 200, 300, ...]
    })
    .run(RunMode::RealTime, run_for)
    .unwrap();
```

## Running the Example

```bash
# Basic run
cargo run --example threading

# With logging to see inter-thread communication
RUST_LOG=INFO cargo run --example threading

# Release mode for performance testing
RUST_LOG=INFO cargo run --release --example threading
```

## Understanding TinyVec Wrapping

Values are wrapped in `TinyVec<[T; 1]>` as they cross thread boundaries. This "burst" container allows multiple values to be batched if they arrive simultaneously.

When chaining `producer().mapper()`:
1. `producer()` outputs `TinyVec<[T; 1]>`
2. `mapper()` receives and wraps again: `TinyVec<[TinyVec<[T; 1]>; 1]>`

Use `.iter().flatten()` to unwrap nested TinyVecs when processing.
