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
- **I/O Adapters**: production-ready [KDB+](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/round_trip) integration for tick data, [CSV](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book), [etcd](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/etcd) key-value store, [ZeroMQ](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/messaging) pub/sub messaging (beta), etc.

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

Wingfoil lets you easily wire up complex business logic, splitting and recombining streams, and altering the frequency of data. I/O adapters make it easy to plug in real data sources and sinks. In this example we load a CSV of AAPL limit orders, maintain an order book using the lobster crate, derive trades and two-way prices, and export back to CSV — all in a few lines:

```rust,ignore
let book = RefCell::new(lobster::OrderBook::default());
let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);
let (fills, prices) = csv_read("aapl.csv", get_time, true)
    .map(move |chunk| process_orders(chunk, &book))
    .split();
let prices_export = prices
    .filter_value(|price: &Option<TwoWayPrice>| !price.is_none())
    .map(|price| price.unwrap())
    .distinct()
    .map(|p| Burst::from([p]))
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


## KDB+ Example

Define a typed struct, implement `KdbDeserialize` to map rows, and stream time-sliced queries directly into your graph:

```rust,ignore
#[derive(Debug, Clone, Default)]
struct Price {
    sym: Sym,
    mid: f64,
}

kdb_read::<Price, _>(
    conn,
    Duration::from_secs(10),
    |(t0, t1), _date, _iter| {
        format!(
            "select time, sym, mid from prices \
             where time >= (`timestamp$){}j, time < (`timestamp$){}j",
            t0.to_kdb_timestamp(),
            t1.to_kdb_timestamp(),
        )
    },
)
.logged("prices", Info)
.run(
    RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
    RunFor::Duration(Duration::from_secs(100)),
)?;
```

[Full example.](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/read/)

## etcd Example

Watch a key prefix, transform values, and write results back — all in a declarative graph:

```rust,ignore
use wingfoil::adapters::etcd::*;
use wingfoil::*;

let conn = EtcdConnection::new("http://localhost:2379");

let round_trip = etcd_sub(conn.clone(), "/source/")
    .map(|burst| {
        burst.into_iter().map(|event| {
            let upper = event.entry.value_str().unwrap_or("").to_uppercase().into_bytes();
            EtcdEntry { key: event.entry.key.replacen("/source/", "/dest/", 1), value: upper }
        })
        .collect::<Burst<EtcdEntry>>()
    })
    .etcd_pub(conn, None, true);

round_trip.run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();
```

[Full example.](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/etcd/)

## Links
- Checkout the [examples](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples)
- Download from [crates.io](https://crates.io/crates/wingfoil/)
- Read the [documentation](https://docs.rs/wingfoil/latest/wingfoil/)
- Review the [benchmarks](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/)
- Download the wingfoil Python module from [pypi.org](https://pypi.org/project/wingfoil/)

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





