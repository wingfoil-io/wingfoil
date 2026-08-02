# Core Examples

Engine concepts, no external services. Everything here runs with `cargo run` and
nothing else installed.

If you are new, read the first three in order — they cover the whole model
between them.

## Start here

| Example | Run | What it teaches |
|---|---|---|
| [`hello_graph`](hello_graph/) | `cargo run -p wingfoil-next --example hello_graph` | Wire → build → run. The same graph in historical and realtime mode. |
| [`ema_crossover`](ema_crossover/) | `cargo run -p wingfoil-next --example ema_crossover` | `fold` / `join` / `map` / `filter` at realistic scale — a backtest shape. |
| [`order_book`](order_book/) | `cargo run -p wingfoil-next --example order_book` | Real state in `fold`: a limit order book, trades and two-way prices. |

## Execution model

| Example | Features | What it teaches |
|---|---|---|
| [`run_mode`](run_mode/) | — | `RunMode::RealTime` vs `HistoricalFrom` over one wiring — the backtest/deploy swap. |
| [`topological_sort`](topological_sort/) | — | Why topological-order scheduling avoids the O(2^N) blow-up that frameworks propagating one path at a time hit when nodes branch and recombine. (Target name: `breadth_first`.) |
| [`feedback`](feedback/) | — | Closing a loop with a `feedback` channel — a control loop a plain DAG cannot express. |
| [`statistics`](statistics/) | — | The `StatisticsOps` trait: EWMA, cumulative and rolling mean/variance/std/min/max/median. |
| [`tracing`](tracing/) | — | The `logged` debug tap and the engine's spans — three instrumentation modes. |

## Execution tiers — `nitro!`

One wiring definition, expanded to an interpreted runner, a fully monomorphized
compiled runner, and a nested compiled island. These three show what that means
and what it costs you in expressiveness.

| Example | Features | What it teaches |
|---|---|---|
| [`odds_evens`](odds_evens/) | — | The minimal split/recombine DAG through `nitro!`; both engines asserted equal. |
| [`dual_mode`](dual_mode/) | — | **The reference for what `nitro!` accepts** — allowed vs rejected wiring, plus the generated code. |
| [`fanout_10x10`](fanout_10x10/) | — | The 100-node benchmark shape, and why the nodes are spelled out rather than looped. |

## Concurrency

| Example | Features | What it teaches |
|---|---|---|
| [`threading`](threading/) | — | A producer sub-graph on a worker thread, feeding the main graph over the channel layer — written out by hand. |
| [`spawn`](spawn/) | — | The same offload via the `spawn` / `spawn_map` combinators, which wrap that plumbing into one call. |
| [`async`](async/) | `async` | The classic `async` example ported: an async producer driving the graph. |
| [`async_source`](async_source/) | `async` | `external` sources — a tokio task pushing into a realtime graph, burst-delivered. |
| [`produce_async_feed`](produce_async_feed/) | `async` | `produce_async` — **timestamped** async values, so the same feed replays deterministically. |

## Graph dynamism

| Example | Features | What it teaches |
|---|---|---|
| [`dynamic`](dynamic/) | `dynamic-graph` | Adding and removing nodes on a **running** graph, between engine cycles. |
| [`demux`](demux/) | `dynamic-graph` | The statically-wired counterpart — the same price book through a fixed slot pool. |

## Running with features

Examples with a feature listed above need it passed explicitly:

```sh
cargo run -p wingfoil-next --example async_source --features async
```

## Elsewhere

- [`../adapters/`](../adapters/) — the I/O edges (files, brokers, stores, telemetry).
- [`../showcase/`](../showcase/) — multi-process end-to-end latency demonstrations.
- [`../../benches/`](../../benches/) — the measured comparison of the three tiers.
