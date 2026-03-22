[![CI](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust.yml/badge.svg)](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust.yml)
[![Crates.io Version](https://img.shields.io/crates/v/wingfoil.svg)](https://crates.io/crates/wingfoil)
[![Docs.rs](https://docs.rs/wingfoil/badge.svg)](https://docs.rs/wingfoil/)
[![PyPI - Version](https://img.shields.io/pypi/v/wingfoil.svg)](https://pypi.org/project/wingfoil/)
[![Documentation Status](https://readthedocs.org/projects/wingfoil/badge/?version=latest)](https://wingfoil.readthedocs.io/en/latest/)

# Wingfoil

Wingfoil is a [blazingly fast](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/), highly scalable 
stream processing framework designed for latency-critical use cases such as electronic trading 
and real-time AI systems.

Wingfoil simplifies receiving, processing and distributing streaming data across your entire stack.

## Features

- **Fast**: Ultra-low latency and high throughput with an efficient [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph) based execution engine.
- **Simple and obvious to use**: Define your graph of calculations; Wingfoil manages its execution.
- **Multi-language**: currently available as a Rust crate and as a beta release, [python package](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-python) with plans to add WASM/JavaScript/TypeScript support.
- **Backtesting**: [Replay historical](https://docs.rs/wingfoil/latest/wingfoil/#historical-vs-realtime) data to backtest and optimise strategies.
- **Async/Tokio**: seamless integration, allows you to [leverage async](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/async) at your graph edges.
- **Multi-threading**: [distribute graph execution](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/threading) across cores.
- **I/O Adapters**: production-ready [KDB+](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/round_trip) integration for tick data, [CSV](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book), [ZeroMQ](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/messaging) pub/sub messaging (beta), etc.


## Quick Start
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

## Links
- Checkout the [examples](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples)
- Download from [crates.io](https://crates.io/crates/wingfoil/)
- Read the [documentation](https://docs.rs/wingfoil/latest/wingfoil/)
- Review the [benchmarks](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/)
- Download the wingfoil Python module from [pypi.org](https://pypi.org/project/wingfoil/)

## Order Book Example

<div align="center">
  <img alt="diagram" src="https://raw.githubusercontent.com/wingfoil-io/wingfoil/refs/heads/main/wingfoil/diagrams/aapl.svg"/>
</div>

Load a CSV of AAPL limit orders, maintain an order book using the lobster crate, derive trades and two-way prices, and export back to CSV — all in a few lines:

```rust,ignore
let book = RefCell::new(lobster::OrderBook::default());
let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);
let (fills, prices) = csv_read_vec("aapl.csv", get_time, true)
    .map(move |chunk| process_orders(chunk, &book))
    .split();
let prices_export = prices
    .filter_value(|price: &Option<TwoWayPrice>| !price.is_none())
    .map(|price| price.unwrap())
    .distinct()
    .csv_write("prices.csv");
let fills_export = fills.csv_write_vec("fills.csv");
Graph::new(vec![prices_export, fills_export], RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
    .print()
    .run()
    .unwrap();
```

[Full example.](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book/)


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





