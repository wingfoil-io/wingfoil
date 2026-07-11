[![CI](https://img.shields.io/github/actions/workflow/status/wingfoil-io/wingfoil/rust-test.yml?branch=main&label=CI)](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust-test.yml)
[![codecov](https://codecov.io/gh/wingfoil-io/wingfoil/graph/badge.svg)](https://codecov.io/gh/wingfoil-io/wingfoil)
[![Crates.io Version](https://img.shields.io/crates/v/wingfoil.svg)](https://crates.io/crates/wingfoil)
[![Docs.rs](https://docs.rs/wingfoil/badge.svg)](https://docs.rs/wingfoil/)
[![PyPI - Version](https://img.shields.io/pypi/v/wingfoil.svg)](https://pypi.org/project/wingfoil/)
[![Documentation Status](https://readthedocs.org/projects/wingfoil/badge/?version=latest)](https://wingfoil.readthedocs.io/en/latest/)
[![npm](https://img.shields.io/npm/v/@wingfoil/client.svg)](https://www.npmjs.com/package/@wingfoil/client)

# Wingfoil

Wingfoil is a [blazingly fast](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/), highly scalable 
stream processing framework designed for latency-critical use cases such as electronic trading 
and real-time AI systems.

It ships with a growing library of production-ready adapters covering tick stores, message buses, market protocols, and observability backends — so you can plug graphs into real data sources and sinks with a single line.

Wingfoil simplifies receiving, processing, distributing and monitoring streaming data across your entire stack.


## Features

- **Fast**: [Ultra low latency](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/) and high throughput with an efficient [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph) based execution engine.
- **Backtesting**: [Replay historical](https://docs.rs/wingfoil/latest/wingfoil/#historical-vs-realtime) data to backtest and optimise strategies.
- **Simple and obvious to use**: Define your graph of calculations; Wingfoil manages its execution.
- **Adapters**: production-ready integrations for [iceoryx2](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/iceoryx2), [KDB+](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/round_trip), [Kafka](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kafka), [Fluvio](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/fluvio), [FIX](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/fix), [ZeroMQ](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/zmq), [etcd](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/etcd), [Prometheus](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/telemetry/prometheus), [OpenTelemetry](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/telemetry/otlp), [CSV](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book), and more.
- **Multi-language**: currently available as a [Rust crate](https://crates.io/crates/wingfoil/), [python package](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-python) and a [TypeScript/JavaScript client](https://www.npmjs.com/package/@wingfoil/client).
- **Graph dynamism**: [rewire your graph](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/dynamic) in response to incoming data.
- **Async/Tokio**: seamless integration, allows you to [leverage async](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/async) at your graph edges.
- **Multi-threading**: [distribute graph execution](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/threading) across cores.


## Quick Start

In this example we build a simple, linear pipeline with all nodes ticking in lock-step.

```rust
use wingfoil::*;
use std::time::Duration;
fn main() {
    let period = Duration::from_secs(1);
    ticker(period)
        .count()
        .map(|i| format!("hello, world {:}", i))
        .print()
        .run(RunMode::RealTime, RunFor::Duration(period*3)
    );
}
```
This output is produced:
```pre
hello, world 1
hello, world 2
hello, world 3
```

## Order Book Example

Wingfoil lets you easily wire up complex business logic, splitting and recombining streams, and modulating the frequency of data. Adapters make it easy to plug in real data sources and sinks. In this example we load a CSV of AAPL limit orders, maintain an order book using the lobster crate, derive trades and two-way prices, and export back to CSV — all in a few lines:

```rust,ignore
let book = RefCell::new(lobster::OrderBook::default());
let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);
let (fills, prices) = csv_read("aapl.csv", get_time, true)
    .map(move |chunk| process_orders(chunk, &book))
    .split();
let prices_export = prices
    .filter_none()
    .distinct()
    .csv_write("prices.csv");
let fills_export = fills.csv_write("fills.csv");
Graph::new(vec![prices_export, fills_export], RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
    .print()
    .run()
    .unwrap();
```

This output is produced:

<div align="center">
  <img alt="diagram" src="https://raw.githubusercontent.com/wingfoil-io/wingfoil/refs/heads/main/wingfoil/diagrams/aapl.svg"/>
</div>

[Full example.](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book/)

## More Examples

Short code snippets for each adapter live in the [examples README](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/README.md). The examples below are all runnable — see each one's `README.md` for setup and commands.

### Core concepts

| Example | Description |
|---|---|
| [`order_book`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book/) | Load NASDAQ AAPL limit orders from CSV, maintain an order book, derive trades and two-way prices, export to CSV. |
| [`breadth_first`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/breadth_first/) | Why wingfoil's BFS execution avoids the O(2^N) node explosion of naive depth-first DAGs. |
| [`run_mode`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/run_mode/) | Swap `RunMode::RealTime` and `RunMode::HistoricalFrom` with the same graph wiring for backtesting. |
| [`async`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/async/) | Integrate Tokio async/await at graph edges (adapters) while keeping the core graph synchronous. |
| [`threading`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/threading/) | Distribute graph execution across worker threads with `producer()` / `mapper()`. |
| [`dynamic`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/dynamic/) | Add and remove nodes at runtime. Includes `demux`, `dynamic-group`, and `dynamic-manual` variants. |
| [`tracing`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/tracing/) | Instrumentation modes (log, tracing, instruments) for event and span handling. |
| [`latency`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/latency/) | Per-hop latency stamping with `Traced<T, L>` and `LatencyReport`, transported over iceoryx2. |

### Adapters

| Example | Description |
|---|---|
| [`kdb`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/) | KDB+ integration: time-sliced reads, cached reads (LRU file cache), and round-trip write/read/validate. |
| [`kafka`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kafka/) | Kafka / Redpanda adapter — subscribe, transform, publish pipeline via `rdkafka`. |
| [`fluvio`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/fluvio/) | Fluvio distributed streaming — subscribe, transform, publish pipeline. |
| [`fix`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/fix/) | FIX 4.4 protocol: self-contained loopback, client, echo server, and live LMAX market data over TLS. |
| [`zmq`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/zmq/) | ZeroMQ pub/sub with direct addressing or etcd-based service discovery. |
| [`etcd`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/etcd/) | etcd key-value store adapter for sub/pub with transformation. |
| [`redis`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/redis/) | Redis adapter — Pub/Sub channels (subscribe, transform, republish) and persistent Streams (snapshot + tail). |
| [`postgres`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/postgres/) | PostgreSQL adapter — time-sliced historical reads and streaming writes, round-trip write/read/validate. |
| [`iceoryx2`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/iceoryx2/) | Zero-copy IPC over shared memory (spin, threaded, signaled polling modes). |
| [`aeron`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/aeron/) | Low-latency Aeron UDP/IPC transport — publish and subscribe to `i64` values with spin and threaded polling modes. |
| [`web`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/web/) | WebSocket adapter streaming synthetic prices and receiving UI events. |
| [`telemetry`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/telemetry/) | Metrics export via Prometheus scraping (pull) and OpenTelemetry OTLP (push). |
| [`augurs`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/augurs/) | augurs time-series toolkit — on-graph forecasting (ETS/MSTL), outlier detection (MAD/DBSCAN), changepoint, seasonality, DTW and clustering over sliding windows. |
| [`statistics`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/statistics/) | Streaming statistics toolkit — EWMA (per-tick and time-decayed), cumulative and rolling mean/variance/std/min/max/median, with count- and time-weighted variants over sample- and time-based windows. |

## Links
- Checkout the [examples](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples)
- Download from [crates.io](https://crates.io/crates/wingfoil/)
- Read the [documentation](https://docs.rs/wingfoil/latest/wingfoil/)
- Review the [benchmarks](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/)
- Download the wingfoil Python module from [pypi.org](https://pypi.org/project/wingfoil/)
- Download the `@wingfoil/client` browser client from [npmjs.com](https://www.npmjs.com/package/@wingfoil/client)

## Get Involved!

We want to hear from you!  Especially if you:
- are interested in [contributing](https://github.com/wingfoil-io/wingfoil/blob/main/CONTRIBUTING.md)
- know of a project that wingfoil would be well-suited for
- would like to request a feature or report a bug
- have any feedback

Please do get in touch:
- ping us on [discord](https://discord.gg/rfGqf3Ff)
- email us at [hello@wingfoil.io](mailto:hello@wingfoil.io)
- submit an [issue](https://github.com/wingfoil-io/wingfoil/issues)
- get involved in the [discussion](https://github.com/wingfoil-io/wingfoil/discussions/)





