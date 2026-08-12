[![CI](https://img.shields.io/github/actions/workflow/status/wingfoil-io/wingfoil/rust-test.yml?branch=main&label=CI&logo=githubactions&logoColor=white)](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust-test.yml)
[![Security audit](https://img.shields.io/github/actions/workflow/status/wingfoil-io/wingfoil/security-audit.yml?branch=main&label=security%20audit&logo=githubactions&logoColor=white)](https://github.com/wingfoil-io/wingfoil/actions/workflows/security-audit.yml)
[![codecov](https://codecov.io/gh/wingfoil-io/wingfoil/graph/badge.svg)](https://codecov.io/gh/wingfoil-io/wingfoil)

[![Crates.io Version](https://img.shields.io/crates/v/wingfoil?logo=rust&logoColor=white)](https://crates.io/crates/wingfoil)
[![Rust docs](https://img.shields.io/docsrs/wingfoil?logo=docsdotrs&logoColor=white&label=rust%20docs)](https://docs.rs/wingfoil/)
[![PyPI - Version](https://img.shields.io/pypi/v/wingfoil?logo=pypi&logoColor=white)](https://pypi.org/project/wingfoil/)
[![npm](https://img.shields.io/npm/v/@wingfoil/client?logo=npm&logoColor=white)](https://www.npmjs.com/package/@wingfoil/client)

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE.txt)
[![Discord](https://img.shields.io/badge/discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/WfZwpQnZUA)

# Wingfoil

Wingfoil is a [blazingly fast](crates/wingfoil/benches/) stream processing
engine for latency-critical systems such as electronic trading and real-time AI.

Wire a graph of calculations once and Wingfoil runs it — interpreted, compiled
into a single monomorphized function, or as compiled islands inside an
interpreted graph. Backtest it over history, then run it live without changing
the wiring.

It ships with production-ready adapters covering tick stores, message buses,
market protocols and observability backends, so graphs plug into real data
sources and sinks in a line.

> **9.0 replaces the engine.** Coming from 8.x, start with the
> [release notes](docs/release-notes/9.0.0.md) and the
> [migration guide](docs/migration.md).


## Features

- **Fast**: [~27 ns](#performance) of engine overhead per node cycle, from a
  topologically sorted [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph)
  execution engine that visits each node once per tick.
- **Three execution tiers, one wiring**: [interpreted, compiled, or a compiled
  island](#execution-tiers) — all derived from the same definition, so they
  cannot drift. Compiled runs [4.4×–37× faster](#performance).
- **Backtesting**: [replay historical data](crates/wingfoil/examples/core/run_mode/)
  deterministically off source-driven engine time, then run the identical graph
  live. Same-instant values ride a single burst — never coalesced, never
  latest-wins, never dropped, in either mode.
- **Adapters**: production-ready integrations for
  [KDB+](crates/wingfoil/examples/adapters/kdb/),
  [PostgreSQL](crates/wingfoil/examples/adapters/postgres/),
  [Kafka](crates/wingfoil/examples/adapters/kafka/),
  [Redis](crates/wingfoil/examples/adapters/redis/),
  [Fluvio](crates/wingfoil/examples/adapters/fluvio/),
  [etcd](crates/wingfoil/examples/adapters/etcd/),
  [ZeroMQ](crates/wingfoil/examples/adapters/zmq/),
  [FIX 4.4](crates/wingfoil/examples/adapters/fix/),
  [iceoryx2](crates/wingfoil/examples/adapters/iceoryx2/),
  [Aeron](crates/wingfoil/examples/adapters/aeron/),
  [WebSocket](crates/wingfoil/examples/adapters/web/),
  [Prometheus](crates/wingfoil/examples/adapters/prometheus/),
  [OpenTelemetry](crates/wingfoil/examples/adapters/otlp/),
  [CSV](crates/wingfoil/examples/adapters/csv/),
  [augurs](crates/wingfoil/examples/adapters/augurs/) and
  [more](#adapters) — one runnable example each.
- **Latency tracing**: [per-hop wall-clock stamps](crates/wingfoil/examples/showcase/)
  aggregating into one report, across shared memory and the wire.
- **Multi-language**: a [Rust crate](https://crates.io/crates/wingfoil/), a
  [Python package](crates/wingfoil-python/) and a
  [TypeScript client](js/).
- **Graph dynamism**: [add and remove nodes](crates/wingfoil/examples/core/dynamism/)
  on a running graph, between cycles.
- **Async/Tokio**: [seamless integration](crates/wingfoil/examples/core/async/)
  at your graph edges.
- **Multi-threading**: [distribute graph execution](crates/wingfoil/examples/core/threading/)
  across cores, with no locks on the graph execution path.
- **Errors are values**: every lifecycle function returns a `Result`; a producer
  error propagates into the graph and aborts the run with context.
- **Extensible without forking**: sources, combinators, statistics and adapters
  are extension traits, and your own ops get interpreted *and* compiled coverage
  from `#[op]`.


## Quick Start

```sh
cargo add wingfoil            # Rust
pip install wingfoil          # Python
npm install @wingfoil/client  # TypeScript client for the web adapter
```

A simple linear pipeline, with all nodes ticking in lock-step:

```rust
use std::time::Duration;
use wingfoil::{RunFor, RunMode};
use wingfoil::prelude::*;

fn main() {
    GraphBuilder::new()
        .ticker(Duration::from_secs(1))
        .count()
        .map(|i| format!("hello, world {i}"))
        .print()
        .build()
        .run(RunMode::RealTime, RunFor::Cycles(3))
        .unwrap();
}
```

This output is produced:

```pre
hello, world 1
hello, world 2
hello, world 3
```


## Order Book Example

Wingfoil lets you wire up complex business logic, splitting and recombining
streams and modulating the frequency of data. Adapters make it easy to plug in
real data sources and sinks. Here we load a CSV of AAPL limit orders, maintain
an order book with the lobster crate, derive trades and two-way prices, and
export both back to CSV:

```rust,ignore
let book = RefCell::new(lobster::OrderBook::default());
let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);

let g = GraphBuilder::new();
let (fills, prices) = csv_read(&g, &source_path, get_time, true, None)?
    .map(move |chunk: &Burst<Message>| process_orders(chunk, &book))
    .split();

let _prices_sink = prices.filter_none().distinct().csv_write(&prices_path)?;
let _fills_sink = fills.csv_write(&fills_path)?;

g.build().run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
```

The frequencies of the inputs and outputs are all different to each other —
messages arrive in same-timestamp bursts, the top of book changes less often,
and trades are sparser still. This output is produced:

<div align="center">
  <img alt="AAPL best bid/ask with fills overlaid" src="crates/wingfoil/examples/core/order_book/aapl.svg"/>
</div>

An hour of market data — 91,998 messages — replays in about a tenth of a
second. [Full example.](crates/wingfoil/examples/core/order_book/)


## Execution tiers

One wiring function, wrapped in `nitro! { fn my_graph(g: &GraphBuilder) -> ... }`,
expands to a module offering all three tiers:

| Tier | Entry point | What it is |
|---|---|---|
| Interpreted | fluent chaining directly, or `my_graph::interpreted()` | One dyn boundary per op; open world — threaded/busy-poll sources, feedback, bursts. |
| Compiled | `my_graph::compiled(run_mode, run_for)` | The whole graph monomorphized into one function, state in locals — fastest, static DAGs. |
| Nested (island) | `my_graph::nested(&g, inputs...)` | A compiled sub-graph mounted as one node of an interpreted graph — hot core compiled, edges stay open. |

Semantics live once, in each op's `cycle` function — the tiers differ only in
how the engine reaches it, so there is no duplicated execution logic behind
those three doors. [`core/dual_mode`](crates/wingfoil/examples/core/dual_mode/)
has the rules governing what a `nitro!` wiring accepts.


## Performance

Read the **ratios**, not the absolute times: these were captured on shared
4-core cloud VMs, each comparison measured back to back in the same run. Full
method, caveats and per-workload tables:
[`benches/README.md`](crates/wingfoil/benches/README.md).

| | Measurement |
|---|---|
| Engine overhead per node cycle | **~27 ns** (10×10 graph, 100 nodes, every node ticking every cycle) |
| Compiled vs interpreted | **4.4×–37× faster** across eight workloads |
| Nested island vs interpreted | **2.2×–10.2× faster** |
| Interpreted vs the legacy engine | **0.56×–0.84×** — the port is faster on all eight |
| vs rxrust / tokio async streams | **~79× / ~134× faster** at depth 10, and the gap grows with depth |

Wingfoil visits every node once per tick, in topological order. Libraries that
propagate along one path at a time re-visit shared nodes once per path — so on a
branch-and-recombine graph their cost doubles with every level while Wingfoil's
stays flat. [`core/topological_sort`](crates/wingfoil/examples/core/topological_sort/)
explains the mechanism in 40 lines.

<div align="center">
  <img alt="Branch/recombine cost by depth: wingfoil flat, rxrust and tokio doubling per level" src="crates/wingfoil/benches/topological_vs_per_path/headline_log.png" width="760"/>
</div>

Where the engine sits against FPGA, kernel-bypass and GC'd stacks — and what is
deliberately *not* claimed — is in
[where wingfoil sits](crates/wingfoil/benches/README.md#where-wingfoil-sits).


## Examples

44 runnable examples, each in its own directory with a README covering what it
teaches, the wiring, and its expected output. Full index:
[`examples/README.md`](crates/wingfoil/examples/README.md).

If you are new, run these three in order — they cover the whole model between
them:

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example hello_graph   # wire → build → run
cargo run --manifest-path crates/wingfoil/Cargo.toml --example ema_crossover # fold/join/map/filter at backtest scale
cargo run --manifest-path crates/wingfoil/Cargo.toml --features csv --example order_book
```

### Core concepts

No services, no feature flags — these run with a plain `cargo run`.

| Example | Description |
|---|---|
| [`hello_graph`](crates/wingfoil/examples/core/hello_graph/) | The smallest complete program: wire, build, run. |
| [`ema_crossover`](crates/wingfoil/examples/core/ema_crossover/) | A backtest-shaped graph — fold, join, map and filter over a price series. |
| [`order_book`](crates/wingfoil/examples/core/order_book/) | Load NASDAQ AAPL limit orders from CSV, maintain an order book, derive trades and two-way prices, write both back out. |
| [`run_mode`](crates/wingfoil/examples/core/run_mode/) | Swap `RunMode::RealTime` and `RunMode::HistoricalFrom` over the same wiring, for backtesting. |
| [`dual_mode`](crates/wingfoil/examples/core/dual_mode/) | One wiring, three execution tiers — interpreted, compiled, and a compiled island — proven to agree. |
| [`topological_sort`](crates/wingfoil/examples/core/topological_sort/) | Why topologically sorted execution avoids the O(2^N) node explosion of naive per-path propagation. |
| [`dynamism`](crates/wingfoil/examples/core/dynamism/) | Add and remove nodes on a running graph — one price book, four wirings. |
| [`feedback`](crates/wingfoil/examples/core/feedback/) | Close a loop between two nodes with `feedback` — a proportional control loop a plain DAG cannot express. |
| [`statistics`](crates/wingfoil/examples/core/statistics/) | Streaming statistics: EWMA, cumulative and rolling mean/variance/std/min/max/median, over sample- and time-based windows. |
| [`async`](crates/wingfoil/examples/core/async/) | Tokio async/await at the graph's edges, with the core graph staying synchronous. |
| [`async_source`](crates/wingfoil/examples/core/async_source/) | An async quote feed driving the graph through an `external` source. |
| [`threading`](crates/wingfoil/examples/core/threading/) | Distribute graph execution across worker threads, with no locks on the execution path. |
| [`spawn`](crates/wingfoil/examples/core/spawn/) | Offload slow work off the graph thread with `spawn` / `spawn_map`. |
| [`tracing`](crates/wingfoil/examples/core/tracing/) | Observability: the `logged` debug tap and the engine's own spans. |
| [`introspect`](crates/wingfoil/examples/core/introspect/) | Read back the graph you wired — text, Mermaid, DOT, JSON or GML. |

### Adapters

One directory per adapter, each behind its cargo feature. See each README for
the service to start and the command to run.

| Example | Description |
|---|---|
| [`kdb`](crates/wingfoil/examples/adapters/kdb/) | KDB+ in three parts: time-sliced reads, LRU-cached reads, and a round-trip write/read/validate. |
| [`postgres`](crates/wingfoil/examples/adapters/postgres/) | PostgreSQL — time-sliced historical reads and streaming writes, round-tripped and asserted to tie out. |
| [`kafka`](crates/wingfoil/examples/adapters/kafka/) | Consume a Kafka topic, transform each record, produce to another. |
| [`fluvio`](crates/wingfoil/examples/adapters/fluvio/) | Fluvio — seed a topic, consume it, transform, write to a second topic, from one `GraphBuilder`. |
| [`redis`](crates/wingfoil/examples/adapters/redis/) | Redis Pub/Sub end to end: publish, subscribe, transform, republish. |
| [`etcd`](crates/wingfoil/examples/adapters/etcd/) | Watch an etcd key prefix, transform the values, write them back under another. |
| [`zmq`](crates/wingfoil/examples/adapters/zmq/) | ZeroMQ pub/sub, with direct addressing or etcd service discovery. |
| [`fix`](crates/wingfoil/examples/adapters/fix/) | FIX 4.4 — an acceptor and an initiator in one process, over a loopback session. |
| [`iceoryx2`](crates/wingfoil/examples/adapters/iceoryx2/) | Zero-copy IPC over shared memory, in spin, threaded and signaled polling modes. |
| [`aeron`](crates/wingfoil/examples/adapters/aeron/) | Low-latency Aeron UDP/IPC transport — publish and subscribe over `aeron:ipc`. |
| [`web`](crates/wingfoil/examples/adapters/web/) | Stream a synthetic mid-price to a browser over WebSocket, and take UI events back in. |
| [`ws`](crates/wingfoil/examples/adapters/ws/) | A reconnecting WebSocket *client* feeding a graph — the transport half of a venue adapter. |
| [`prometheus`](crates/wingfoil/examples/adapters/prometheus/) | Serve `GET /metrics` in the Prometheus text format for a scraper or Grafana. |
| [`otlp`](crates/wingfoil/examples/adapters/otlp/) | Push stream values to an OpenTelemetry backend over OTLP. |
| [`telemetry`](crates/wingfoil/examples/adapters/telemetry/) | The two exporters side by side — pull-based scraping vs push — with a Grafana stack. |
| [`csv`](crates/wingfoil/examples/adapters/csv/) | Replay a CSV as a deterministic historical burst stream, transform it, write it back. The one to read first — it needs no server. |
| [`lines`](crates/wingfoil/examples/adapters/lines/) | Line-oriented files in both directions — the smallest complete I/O edge. |
| [`augurs`](crates/wingfoil/examples/adapters/augurs/) | On-graph time-series analysis with Grafana's augurs: forecasting, outliers, changepoints, seasonality, DTW, clustering. |

### Showcase

| Example | Description |
|---|---|
| [`latency`](crates/wingfoil/examples/showcase/latency/) | A two-process pipeline over iceoryx2 with per-hop stamping and an end-of-run report. |
| [`trading_e2e`](crates/wingfoil/examples/showcase/trading_e2e/) | Browser to live venue and back: WebSocket in, shared memory across processes, FIX/TLS out, with Grafana dashboards over the whole path. |


## Links

- Explore the [examples](crates/wingfoil/examples/)
- Read the [release notes](docs/release-notes/)
- Compare the field: [stream processing, dataflow and trading frameworks](docs/comparison.md)
- Browse the [crates](crates/)
- Read the [benchmarks](crates/wingfoil/benches/)
- Use it from Python: [`wingfoil-python`](crates/wingfoil-python/)
- Use it from the browser: [`@wingfoil/client`](js/)
- See [CONTRIBUTING](CONTRIBUTING.md) to build, test and contribute


## Get Involved!

We want to hear from you! Especially if you:
- are interested in [contributing](CONTRIBUTING.md)
- know of a project that Wingfoil would be well-suited for
- would like to request a feature or report a bug
- have any feedback

Please do get in touch:
- ping us on [discord](https://discord.gg/WfZwpQnZUA)
- email us at [hello@wingfoil.io](mailto:hello@wingfoil.io)
- submit an [issue](https://github.com/wingfoil-io/wingfoil/issues)
- get involved in the [discussion](https://github.com/wingfoil-io/wingfoil/discussions/)
