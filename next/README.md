# Wingfoil Next

Wingfoil Next is a blazingly fast, highly scalable stream processing engine
designed for latency-critical use cases such as electronic trading and
real-time AI systems.

You describe your graph of calculations once, in a single fluent wiring, and
choose how to run it: an **interpreted** engine for a fully open, dynamic
world; a fully monomorphized **compiled** runner for maximum throughput; or
**nested** compiled islands mounted inside an interpreted graph. Every tier is
derived from the same definition, so they cannot drift — there is no
duplicated execution logic anywhere.

Wingfoil simplifies receiving, processing, distributing and monitoring
streaming data across your entire stack.


## Features

- **Fast**: ultra low latency and high throughput from an efficient
  [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph)-based execution
  engine, with a `compiled()` tier that monomorphizes the whole graph into one
  function for the compiler to optimise across node boundaries.
- **Three execution tiers**: run any graph interpreted (open-world, dynamic),
  fully compiled (static, fastest), or as compiled islands nested inside an
  interpreted graph — all from one wiring definition.
- **Backtesting**: replay historical data deterministically to backtest and
  optimise strategies, then swap to realtime with the same graph wiring.
- **Lossless**: same-instant values ride a single burst — never coalesced,
  never latest-wins — identically in realtime and historical replay.
- **Fallible everywhere**: every lifecycle function returns a `Result`; errors
  abort the run with context and cleanup still runs.
- **Simple to use**: define your graph of calculations; Wingfoil manages its
  execution.
- **Adapters**: integrations for CSV, etcd, the augurs time-series toolkit,
  and line-oriented files, with async/Tokio at your graph edges.
- **Multi-threading**: distribute graph execution across threads through the
  channel layer.
- **Extensible**: add sources, combinators, statistics and adapters as
  extension traits; your own ops get interpreted *and* compiled coverage with
  `#[op]`, with no macro table to edit.


## Quick Start

In this example we build a simple, linear pipeline with all nodes ticking in
lock-step.

```rust
use std::time::Duration;
use wingfoil::{RunFor, RunMode};
use wingfoil_next::prelude::*;

fn main() {
    let g = GraphBuilder::new();
    g.ticker(Duration::from_secs(1))
        .count()
        .map(|i| format!("hello, world {i}"))
        .print();

    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();
}
```

This output is produced:

```pre
hello, world 1
hello, world 2
hello, world 3
```


## A Worked Example

Wingfoil lets you wire up complex business logic, splitting and recombining
streams and modulating the frequency of data. Here a price stream is folded
into fast and slow EMAs, recombined into a crossover signal, and gated so it
only fires when the signal *changes*:

```rust,ignore
let g = GraphBuilder::new();
let price = g.ticker(Duration::from_millis(1)).map(next_price);

// Fast and slow EMAs over the same price stream, recombined into a signal.
let fast = price.fold((0.0, false), ema(0.30)).map(|s| s.0);
let slow = price.fold((0.0, false), ema(0.05)).map(|s| s.0);
let signal = fast.join(&slow, |f, s| f > s);

// Emit only when the crossover state changes.
signal
    .fold((false, false), |st, s| { st.0 = st.1; st.1 = *s; })
    .filter(|st| st.0 != st.1)
    .print();

let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5_000)).unwrap();
```

See the full [`order_book`](crates/wingfoil-next/examples/order_book/) and
[`ema_crossover`](crates/wingfoil-next/examples/ema_crossover.rs) examples.


## Execution tiers

One wiring function, wrapped in `graph! { fn my_graph(g: &GraphBuilder) -> ... }`,
expands to a module offering all three tiers:

| Tier | Entry point | What it is |
|---|---|---|
| Interpreted | fluent chaining directly, or `my_graph::interpreted()` | One dyn boundary per op; open world — threaded/busy-poll sources, feedback, bursts. |
| Compiled | `my_graph::compiled(run_mode, run_for)` | The whole graph monomorphized into one function, state in locals — fastest, static DAGs. |
| Nested (island) | `my_graph::nested(&g, inputs...)` | A compiled sub-graph mounted as one node of an interpreted graph — hot core compiled, edges stay open. |


## More Examples

Every example is runnable with `cargo run -p wingfoil-next --example <name>`
(add `--features <name>` for adapter examples).

### Core concepts

| Example | Description |
|---|---|
| [`hello_graph`](crates/wingfoil-next/examples/hello_graph.rs) | Smallest graph: a ticker counted and formatted, run historical (instant) then realtime. |
| [`order_book`](crates/wingfoil-next/examples/order_book/) | Maintain a limit order book in `fold` state, derive trades and two-way prices. |
| [`ema_crossover`](crates/wingfoil-next/examples/ema_crossover.rs) | Backtest-shaped: a price walk, fast/slow EMAs, and golden/death-cross signals on state change. |
| [`breadth_first`](crates/wingfoil-next/examples/breadth_first/) | Why breadth-first execution avoids the node explosion of naive depth-first DAGs. |
| [`run_mode`](crates/wingfoil-next/examples/run_mode/) | Swap `RunMode::RealTime` and `RunMode::HistoricalFrom` with the same graph wiring. |
| [`feedback`](crates/wingfoil-next/examples/feedback/) | Close a loop between nodes with a `feedback` channel — a control loop a plain DAG can't express. |
| [`threading`](crates/wingfoil-next/examples/threading/) | Run a producer sub-graph on its own thread, feeding the main graph over the channel layer. |
| [`async`](crates/wingfoil-next/examples/async/) | Drive a graph from an async/Tokio producer of timestamped values at the graph edge. |
| [`statistics`](crates/wingfoil-next/examples/statistics/) | Streaming statistics toolkit — EWMA, cumulative and rolling mean/variance/std/min/max/median. |
| [`odds_evens`](crates/wingfoil-next/examples/odds_evens.rs) | Split a counter by parity into two branches and merge back — the split-and-recombine DAG. |
| [`dual_mode`](crates/wingfoil-next/examples/dual_mode.rs) | One `graph!` wiring expands to both an interpreted and a fully compiled runner. |
| [`fanout_10x10`](crates/wingfoil-next/examples/fanout_10x10.rs) | A 10×10 fan-out graph expressed through `graph!`, the benchmark shape. |

### Adapters

| Example | Description |
|---|---|
| [`csv_adapter`](crates/wingfoil-next/examples/csv_adapter.rs) | Replay a CSV as a deterministic historical burst stream, transform each row, write back to CSV. |
| [`etcd_adapter`](crates/wingfoil-next/examples/etcd_adapter.rs) | Watch an etcd key prefix, transform values, and write the result back. |
| [`augurs_adapter`](crates/wingfoil-next/examples/augurs_adapter.rs) | On-graph forecasting, outlier / changepoint / season detection, DTW and clustering over sliding windows with the augurs toolkit. |
| [`lines_adapter`](crates/wingfoil-next/examples/lines_adapter.rs) | Dependency-free line-oriented file adapter — replay a text file, transform it, write it out. |
| [`async_source`](crates/wingfoil-next/examples/async_source.rs) | Bridge an async producer of timestamped values into a wingfoil-next graph. |


## Links

- Explore the [examples](crates/wingfoil-next/examples/)
- Read the [benchmarks](crates/wingfoil-next/benches/)
- See [CONTRIBUTING](CONTRIBUTING.md) to build, test and contribute


## Get Involved!

We want to hear from you! Especially if you:
- are interested in [contributing](CONTRIBUTING.md)
- know of a project that Wingfoil would be well-suited for
- would like to request a feature or report a bug
- have any feedback

Please do get in touch:
- ping us on [discord](https://discord.gg/rfGqf3Ff)
- email us at [hello@wingfoil.io](mailto:hello@wingfoil.io)
- submit an [issue](https://github.com/wingfoil-io/wingfoil/issues)
- get involved in the [discussion](https://github.com/wingfoil-io/wingfoil/discussions/)
