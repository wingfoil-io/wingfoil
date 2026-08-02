[![CI](https://img.shields.io/github/actions/workflow/status/wingfoil-io/wingfoil/rust-test.yml?branch=main&label=CI)](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust-test.yml)
[![codecov](https://codecov.io/gh/wingfoil-io/wingfoil/graph/badge.svg)](https://codecov.io/gh/wingfoil-io/wingfoil)

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
- **Adapters**: sixteen production-ready integrations — PostgreSQL, KDB+,
  Kafka, Redis, etcd, Fluvio, ZeroMQ, FIX 4.4, iceoryx2, Aeron, WebSocket,
  Prometheus, OpenTelemetry, CSV, augurs and line-oriented files — with
  async/Tokio at your graph edges, plus an LRU file cache for time-sliced
  readers. One runnable example each,
  [indexed here](crates/wingfoil-next/examples/adapters/).
- **Multi-language**: a Rust crate, a [Python
  package](crates/wingfoil-next-python/) exposing the same graph model,
  adapters and latency surface, and a [TypeScript/JavaScript client](js/) for
  the web adapter.
- **Latency tracing**: per-hop wall-clock stamps that survive a process hop and
  aggregate into one report — see
  [`showcase/`](crates/wingfoil-next/examples/showcase/).
- **Graph dynamism**: add and remove nodes on a
  [running graph](crates/wingfoil-next/examples/core/dynamic/), between cycles.
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
use wingfoil_next::{RunFor, RunMode};
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

See the full [`order_book`](crates/wingfoil-next/examples/core/order_book/) and
[`ema_crossover`](crates/wingfoil-next/examples/core/ema_crossover/) examples.


## Execution tiers

One wiring function, wrapped in `nitro! { fn my_graph(g: &GraphBuilder) -> ... }`,
expands to a module offering all three tiers:

| Tier | Entry point | What it is |
|---|---|---|
| Interpreted | fluent chaining directly, or `my_graph::interpreted()` | One dyn boundary per op; open world — threaded/busy-poll sources, feedback, bursts. |
| Compiled | `my_graph::compiled(run_mode, run_for)` | The whole graph monomorphized into one function, state in locals — fastest, static DAGs. |
| Nested (island) | `my_graph::nested(&g, inputs...)` | A compiled sub-graph mounted as one node of an interpreted graph — hot core compiled, edges stay open. |


## Examples

Around 40 runnable examples, each in its own directory with a README covering
what it teaches, the wiring, and its expected output. Full index:
[`examples/README.md`](crates/wingfoil-next/examples/README.md).

```sh
cargo run -p wingfoil-next --example <name>                      # core examples
cargo run -p wingfoil-next --example <name> --features <feature> # anything gated
```

### Start here

Three examples, in order — they cover the whole model between them.

```sh
cargo run -p wingfoil-next --example hello_graph      # wire → build → run
cargo run -p wingfoil-next --example ema_crossover    # fold/join/map/filter at backtest scale
cargo run -p wingfoil-next --example order_book       # real state in fold
```

Then pick a direction: [`adapters/`](crates/wingfoil-next/examples/adapters/) to
plug in real data, [`core/dual_mode`](crates/wingfoil-next/examples/core/dual_mode/)
for the execution tiers, or [`core/run_mode`](crates/wingfoil-next/examples/core/run_mode/)
to backtest.

### Core concepts — [index](crates/wingfoil-next/examples/core/)

| Example | Description |
|---|---|
| [`hello_graph`](crates/wingfoil-next/examples/core/hello_graph/) | Smallest graph: a ticker counted and formatted, run historical (instant) then realtime. |
| [`ema_crossover`](crates/wingfoil-next/examples/core/ema_crossover/) | Backtest-shaped: a price walk, fast/slow EMAs, and golden/death-cross signals on state change. |
| [`order_book`](crates/wingfoil-next/examples/core/order_book/) | Maintain a limit order book in `fold` state, derive trades and two-way prices. |
| [`run_mode`](crates/wingfoil-next/examples/core/run_mode/) | Swap `RunMode::RealTime` and `RunMode::HistoricalFrom` with the same graph wiring. |
| [`breadth_first`](crates/wingfoil-next/examples/core/breadth_first/) | Why breadth-first execution avoids the node explosion of naive depth-first DAGs. |
| [`feedback`](crates/wingfoil-next/examples/core/feedback/) | Close a loop between nodes with a `feedback` channel — a control loop a plain DAG can't express. |
| [`statistics`](crates/wingfoil-next/examples/core/statistics/) | Streaming statistics toolkit — EWMA, cumulative and rolling mean/variance/std/min/max/median. |
| [`tracing`](crates/wingfoil-next/examples/core/tracing/) | The `logged` debug tap and the engine's spans — three instrumentation modes. |
| [`odds_evens`](crates/wingfoil-next/examples/core/odds_evens/) | Split a counter by parity into two branches and merge back — the split-and-recombine DAG, through `nitro!`. |
| [`dual_mode`](crates/wingfoil-next/examples/core/dual_mode/) | One `nitro!` wiring expands to both an interpreted and a fully compiled runner — and the rules governing what it accepts. |
| [`fanout_10x10`](crates/wingfoil-next/examples/core/fanout_10x10/) | A 10×10 fan-out graph expressed through `nitro!`, the benchmark shape. |
| [`threading`](crates/wingfoil-next/examples/core/threading/) | Run a producer sub-graph on its own thread, feeding the main graph over the channel layer. |
| [`spawn`](crates/wingfoil-next/examples/core/spawn/) | The same offload through the `spawn` / `spawn_map` combinators. |
| [`async`](crates/wingfoil-next/examples/core/async/) | Drive a graph from an async/Tokio producer at the graph edge. |
| [`async_source`](crates/wingfoil-next/examples/core/async_source/) | `external` sources — a tokio task pushing into a realtime graph, burst-delivered. |
| [`produce_async_feed`](crates/wingfoil-next/examples/core/produce_async_feed/) | `produce_async` — timestamped async values, so the same feed replays deterministically. |
| [`dynamic`](crates/wingfoil-next/examples/core/dynamic/) | Add and remove nodes on a **running** graph, between engine cycles. |
| [`demux`](crates/wingfoil-next/examples/core/demux/) | The statically-wired counterpart — the same price book through a fixed slot pool. |

### Adapters — [index](crates/wingfoil-next/examples/adapters/)

| Example | Feature | Description |
|---|---|---|
| [`lines`](crates/wingfoil-next/examples/adapters/lines/) | `async` | Dependency-free line-oriented file adapter — the smallest complete I/O edge. |
| [`csv`](crates/wingfoil-next/examples/adapters/csv/) | `csv` | Replay a CSV as a deterministic historical burst stream, transform each row, write back to CSV. |
| [`augurs`](crates/wingfoil-next/examples/adapters/augurs/) | `augurs` | On-graph forecasting, outlier / changepoint / season detection, DTW and clustering over sliding windows. |
| [`zmq`](crates/wingfoil-next/examples/adapters/zmq/) | `zmq` | Brokerless ZeroMQ pub/sub, with connection status as a stream. |
| [`kafka`](crates/wingfoil-next/examples/adapters/kafka/) | `kafka` | Kafka / Redpanda — consume, transform, produce. |
| [`fluvio`](crates/wingfoil-next/examples/adapters/fluvio/) | `fluvio` | Fluvio distributed streaming — subscribe, transform, publish. |
| [`redis`](crates/wingfoil-next/examples/adapters/redis/) | `redis` | Redis Pub/Sub — subscribe, transform, republish. |
| [`etcd`](crates/wingfoil-next/examples/adapters/etcd/) | `etcd` | Watch an etcd key prefix, transform values, and write the result back. |
| [`iceoryx2`](crates/wingfoil-next/examples/adapters/iceoryx2/) | `iceoryx2` | Zero-copy IPC over shared memory. |
| [`aeron`](crates/wingfoil-next/examples/adapters/aeron/) | `aeron` | Low-latency Aeron UDP/IPC, plus a status-driven circuit breaker. |
| [`kdb`](crates/wingfoil-next/examples/adapters/kdb/) | `kdb` | KDB+ — time-sliced reads, an LRU file cache, and a write/read/validate round trip. |
| [`postgres`](crates/wingfoil-next/examples/adapters/postgres/) | `postgres` | PostgreSQL — time-sliced historical reads and streaming writes. |
| [`fix`](crates/wingfoil-next/examples/adapters/fix/) | `fix` | FIX 4.4 loopback — acceptor and initiator in one process, no external engine. |
| [`web`](crates/wingfoil-next/examples/adapters/web/) | `web` | WebSocket — stream prices to a browser, receive UI events back. |
| [`prometheus`](crates/wingfoil-next/examples/adapters/prometheus/) | `prometheus` | Serve `/metrics` for scraping (pull). |
| [`otlp`](crates/wingfoil-next/examples/adapters/otlp/) | `otlp,prometheus` | Push over OTLP *and* serve `/metrics` (push + pull). |
| [`telemetry`](crates/wingfoil-next/examples/adapters/telemetry/) | — | The shared Docker harness (Prometheus, Grafana, Alloy) both exporters scrape into. |

### Showcase — [index](crates/wingfoil-next/examples/showcase/)

| Example | Description |
|---|---|
| [`latency`](crates/wingfoil-next/examples/showcase/latency/) | Per-hop latency stamping with `latency_stages!` and `Traced<T, L>`, across an iceoryx2 shared-memory hop. |
| [`latency_e2e`](crates/wingfoil-next/examples/showcase/latency_e2e/) | Nine stages, browser to venue and back — WebSocket → iceoryx2 → FIX/TLS, with Prometheus, Grafana and Tempo. |


## Links

- Explore the [examples](crates/wingfoil-next/examples/)
- Browse the [crates](crates/)
- Read the [benchmarks](crates/wingfoil-next/benches/)
- Use it from Python: [`wingfoil-next-python`](crates/wingfoil-next-python/)
- Use it from the browser: [`@wingfoil/client`](js/)
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
