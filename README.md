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

- **Fast**: Ultra-low latency and high throughput with a efficent [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph) based execution engine.  
- **Simple and obvious to use**: Define your graph of calculations; Wingfoil manages its execution.  
- **Multi-language**: currently available as a Rust crate and as a beta release, [python package](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-python) with plans to add WASM/JavaScript/TypeScript support.
- **Backtesting**: [Replay historical](https://docs.rs/wingfoil/latest/wingfoil/#historical-vs-realtime) data to backtest and optimise strategies.
- **Async/Tokio**: seamless integration, allows you to [leverage async](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/async) at your graph edges.
- **Multi-threading**: [distribute graph execution](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/async) across cores.


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

You can download from [crates.io](https://crates.io/crates/wingfoil/),
read the [documentation](https://docs.rs/wingfoil/latest/wingfoil/), 
review the [benchmarks](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/) 
or jump straight into [one of the examples](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/order_book).
You can download the wingfoil Python module from [pypi](https://pypi.org/project/wingfoil/).

## Get Involved!

We want to hear from you!  Especially if you:
- are interested in [contributing](https://github.com/wingfoil-io/wingfoil/blob/main/CONTRIBUTING.md)
- know of a project that wingfoil would be well-suited for
- would like to request a feature or report a bug
- have any feedback

Please email us at [hello@wingfoil.io](mailto:hello@wingfoil.io), submit an [issue](https://github.com/wingfoil-io/wingfoil/issues) or get involved in the [discussion](https://github.com/wingfoil-io/wingfoil/discussions/).





