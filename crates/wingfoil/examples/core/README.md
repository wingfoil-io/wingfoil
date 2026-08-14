# Core Examples

Engine concepts, no external services. Everything here runs with `cargo run` and
nothing else installed — `order_book` is the one that needs a feature flag
(`--features csv`), and it still reads a file that ships with the repository.

If you are new, read the first three in order — they cover the whole model
between them.

## Start here

| Example | Run | What it teaches |
|---|---|---|
| [`hello_graph`](hello_graph/) | `cargo run -p wingfoil --example hello_graph` | Wire → build → run. The same graph in historical and realtime mode. |
| [`ema_crossover`](ema_crossover/) | `cargo run -p wingfoil --example ema_crossover` | `fold` / `join` / `map` / `filter` at realistic scale — a backtest shape. |
| [`order_book`](order_book/) | `cargo run -p wingfoil --features csv --example order_book` | Real market data in and out: a CSV of AAPL limit orders, a book, trades and two-way prices at three different frequencies. |

## Execution model

| Example | Features | What it teaches |
|---|---|---|
| [`run_mode`](run_mode/) | — | `RunMode::RealTime` vs `HistoricalFrom` over one wiring — the backtest/deploy swap. |
| [`odds_evens`](odds_evens/) | — | Split and recombine — a counter fanned out by parity and merged back, builder-less (free `ticker`, run directly, output via `logged`). |
| [`topological_sort`](topological_sort/) | — | Why topological-order scheduling avoids the O(2^N) blow-up that frameworks propagating one path at a time hit when nodes branch and recombine. (Target name: `breadth_first`.) |
| [`feedback`](feedback/) | — | Closing a loop with a `feedback` channel — a control loop a plain DAG cannot express. |
| [`statistics`](statistics/) | `statistics` | The `StatisticsOps` trait: EWMA, cumulative and rolling mean/variance/std/min/max/median. |
| [`tracing`](tracing/) | — | The `logged` debug tap and the engine's spans — three instrumentation modes. |
| [`introspect`](introspect/) | — | Seeing the graph you wired: `snapshot()` to text / Mermaid / Graphviz / JSON, with active and passive edges drawn apart. |

## Execution tiers — Nitro (`nitro!`)

One wiring definition, expanded to an interpreted runner, a fully monomorphized
compiled runner, and a nested compiled island. This is what that means and what
it costs you in expressiveness.

| Example | Features | What it teaches |
|---|---|---|
| [`dual_mode`](dual_mode/) | — | A split/recombine DAG through `nitro!` with both engines asserted equal, **the reference for what the macro accepts** — allowed vs rejected wiring, choosing an engine with `run(tier, ..)`, plus the generated code. |

The measured cost of each tier is in [`../../benches/`](../../benches/) —
`tiers.rs` runs this shape and the 100-node fan-out through all three.

## Concurrency

| Example | Features | What it teaches |
|---|---|---|
| [`threading`](threading/) | — | A producer sub-graph on a worker thread, feeding the main graph over the channel layer — written out by hand. |
| [`spawn`](spawn/) | — | The same offload via the `spawn` / `spawn_map` combinators, which wrap that plumbing into one call. |
| [`async`](async/) | `async` | The classic `async` example ported: `produce_async` — an async producer of **timestamped** values driving the graph, in both modes off one definition. |
| [`async_source`](async_source/) | `async` | `external` sources — a tokio task pushing into a realtime graph, burst-delivered. |

## Graph dynamism

Four wirings of one problem — a price book over instruments that come and go —
sharing a scenario and a parity oracle. Read
[`dynamism/`](dynamism/) first for the map and for when to reach for which.

| Example | Features | What it teaches |
|---|---|---|
| [`dynamic_group`](dynamism/dynamic_group/) | `dynamic-graph` | Adding and removing nodes on a **running** graph, between engine cycles. (Target name: `dynamic`.) |
| [`dynamic_manual`](dynamism/dynamic_manual/) | `dynamic-graph` | The same splicing driven by hand — `add_upstream` / `remove` from the `run_dynamic` hook. |
| [`demux_it`](dynamism/demux_it/) | `dynamic-graph` | The statically-wired counterpart — the same price book through a fixed slot pool, routing each item of a burst. (Target name: `demux`.) |
| [`demux_map`](dynamism/demux_map/) | `dynamic-graph` | The single-value demux: one routed value per cycle, and what that constrains. |

## Running with features

Examples with a feature listed above need it passed explicitly:

```sh
cargo run -p wingfoil --example async_source --features async
```

## Elsewhere

- [`../adapters/`](../adapters/) — the I/O edges (files, brokers, stores, telemetry).
- [`../showcase/`](../showcase/) — multi-process end-to-end latency demonstrations.
- [`../../benches/`](../../benches/) — the measured comparison of the three tiers.
