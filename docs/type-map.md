# The type map

The middle altitude the other docs skip: which types carry the engine, and how
they contain each other. [`wingfoil-architecture.md`](wingfoil-architecture.md)
is the level above (the decisions); the rustdoc is the level below (every field
justified where it is declared — the field docs are the source of truth, and
this page deliberately does not repeat them). Read this when you know *why* the
engine is shaped this way and need to know *what to grep for*.

Types are grouped by layer, outermost first. Each name links to the file that
defines it.

## How they contain each other

```
GraphBuilder ─┬─ wraps ──▶ Builder ── build() ──▶ Runner ── holds ──▶ Kernel ── holds ──▶ TimeQueue<usize>
  Stream<T> ──┘  (same Rc)     │                    │
                               └── mints ──▶ Handle<T> ──▶ SlotRef<T>   (the value slots)

other threads ──▶ ChannelSender / PooledSender / ExternalSource ── KernelWaker ──▶ Kernel
```

One run, end to end: `GraphBuilder`/`Stream` wire ops into the `Builder`;
`build()` computes edges and layers and yields a `Runner` holding a `Kernel`.
Each cycle, `Kernel::begin_cycle` advances engine time and names the due
frontier; the sparse dispatch drains dirty nodes in `(layer, index)` order,
each one crossing a single dyn boundary into the same monomorphized
`Op::cycle` the compiled tier would inline; `Tick` results land in slots and
propagate. Producers on other threads feed in through the channel layer,
waking the kernel; `feedback` re-enters at `time + 1` through a `TimeQueue`.

## Vocabulary — [`op.rs`](../crates/wingfoil/src/op.rs)

| Type | What it is |
|---|---|
| `Op` | The node contract: `Cfg` (construction-time config, closures included), `State` (engine-owned), `In<'a>` (typed inputs, assembled by the engine), `Out`, `const ACTIVATION`, and `cycle()` plus optional `start`/`stop`/`teardown`. Semantics as an associated *function* — the one decision everything follows from. |
| `Tick<T>` | The outcome of one cycle: `Value` (update slot and tick downstream), `Silent` (update slot only — what `delay` needs), `Quiet` (nothing). |
| `Activation` | Scheduling behaviour as a `const`: `NONE` / `SCHEDULES` / `THREADED` / `ALWAYS`. Read at wiring time interpreted, folded into the dispatch condition compiled. |
| `Ctx<'a>` | The op's entire view of the engine: engine time, the lazy wall snap, run metadata, and *self*-scheduling. Deliberately narrow — that is what lets a composite island drive the same op. |

## Wiring — [`fluent.rs`](../crates/wingfoil/src/fluent.rs)

| Type | What it is |
|---|---|
| `GraphBuilder` | A graph under construction: `Rc<RefCell<Builder>>` plus a `built` flag that poisons wiring after `build()`. The `Rc` is why it — and everything wired from it — is `!Send`: a graph lives on one thread, by type. Extension point: `GraphBuilder::source`. |
| `Stream<T>` | A typed wiring-time reference to one node's output; holds no data. Combinators are extension traits (`SourceOps`, `StreamOps`, `StatisticsOps`, one per adapter), never inherent methods; they all go through `Stream::wire`. |
| `Upstream` | The type-erased edge descriptor used when declaring a node's inputs. |

## Interpreted engine — [`interp.rs`](../crates/wingfoil/src/interp.rs)

| Type | What it is |
|---|---|
| `Builder` | The accumulator behind `GraphBuilder`: per node, the dyn-adapted cycle/lifecycle closures, the op's `Cfg`+`State`, its activation and edges, and a value slot. The engine owns everything the op does not. |
| `Runner` | Executes the built graph. Sparse dirty-list dispatch: seed from busy-poll nodes plus `Kernel::due`, propagate the tick frontier through the active-downstream lists, drain in `(layer, index)` order — glitch-free, single-fire, cost proportional to nodes that fire. `!Send`, and knows whether it is re-runnable. |
| `Dispatch` | Which loop `run` uses: `Sparse` (default) or `FullSweep`, the `O(N)` reference oracle kept for tying the two out. |
| `Handle<T>` / `AsHandle<T>` | A typed node index, stamped with its builder's id so a handle used against the wrong `Runner` asserts instead of reading the wrong slot. `AsHandle` unifies `Handle` and `Stream` (by value and by reference) for `Runner::value`. |
| `SlotRef<T>` | The frozen access boundary to a value slot — ops only `borrow()`/`borrow_mut()`, never the concrete cell, so the store can become an arena later without touching capture sites. |
| `ExternalSource<T>` | Producer half of an `external` source: send a value from any thread, wake the kernel. |
| `FeedbackSink<T>` | Write end of a `feedback` edge: values queue and the paired source node emits them on the *next* cycle, which is what breaks the dependency cycle. |
| `StopHandle` | Wraps any guard whose `Drop` stops a producer; held for the run, dropped at teardown. |
| `Extension` / `LiveStream<T>` / `StreamStore<K, V>` / `DemuxEvent` | The `dynamic-graph` surface: runtime splice and remove (removal tombstones the slot so old `Handle`s stay valid), and keyed sub-streams appearing at runtime via `demux`. |

## Kernel — [`runtime/`](../crates/wingfoil/src/runtime)

| Type | What it is |
|---|---|
| `Kernel` | The minimal engine core all three tiers drive: engine `time` (source-driven, deterministic under replay), the lazy per-cycle `wall_time` snap, the scheduled-callback `TimeQueue`, run bounds, and the `due` frontier. The interpreted `Runner` holds one; `compiled()` stack-allocates its own; an island borrows the outer one. |
| `KernelWaker` / `ReadyReceiver` | The wake channel producer threads use to mark a node dirty and un-park a realtime kernel. |
| `TimerPolicy` | How a realtime kernel waits for the next callback: `Park` (OS sleep) or `SpinAhead` (sleep until a guard before the deadline, then busy-spin — a core traded for scheduler jitter). |
| `TimeQueue<T>` | The `(value, time)` scheduler behind the kernel, `delay` and `feedback`: earliest instant held out of a `BTreeMap`, FIFO within an instant, **dedup by design**, bounded on `PartialEq` so `f64` flows through. See "The rules that bite". |
| `NanoTime` | The nanosecond timestamp; `ZERO` is the epoch and the conventional backtest start. |
| `RunMode` / `RunFor` | `RealTime` vs `HistoricalFrom(t)`; stop after a `Duration` (engine time), a cycle count, or `Forever`. |
| `Burst<T>` | Same-instant values delivered atomically — a `TinyVec<[T; 1]>`, so the single-value common case allocates nothing. Never coalesced, never dropped. |

## Thread boundary — [`channel.rs`](../crates/wingfoil/src/channel.rs)

| Type | What it is |
|---|---|
| `Message<T>` | The in-process envelope: `Value`, `ValueAt` (deterministic replay time), `Checkpoint` (progress without a value, so a quiet channel does not stall a replay), `EndOfStream`, `Error` (aborts the run with node context). |
| `ChannelSender<T>` | The `Clone + Send` write end of `channel` — an mpsc sender (bounded blocking = the backpressure) plus a `KernelWaker`. The only sanctioned way another thread reaches a running graph. |

## Pool — [`pool.rs`](../crates/wingfoil/src/pool.rs)

The zero-allocation ingress path for large payloads, mirroring iceoryx2's
loan/write/send protocol.

| Type | What it is |
|---|---|
| `PooledSender<T>` | Producer half of `pooled_channel`: a bounded buffer pool plus the sender. `loan()` blocks once `capacity` buffers are in flight — the pool *is* the backpressure. |
| `PoolLoan<T>` | A writable loan of one recycled buffer: unique owner, so it crosses the thread boundary with no refcount; its `Drop` returns the buffer to the pool. |
| `Pooled<T>` | The graph-side handle: non-atomic (`Rc`) sharing, so routing ops clone by refcount bump; `PartialEq` is pointer identity (what `delay`/`feedback` dedup wants); `Default` is the empty pre-first-tick handle. |
