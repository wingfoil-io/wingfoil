# Wingfoil

Wingfoil is a [blazingly fast](https://crates.io/crates/wingfoil/benches/), highly scalable 
stream processing framework designed for latency-critical use cases such as electronic trading 
and real-time AI systems.

## Features

- **Fast**: Ultra-low latency and high throughput with a efficent [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph) based execution engine.  
- **Simple and obvious to use**: Define your graph of calculations; Wingfoil manages it's execution.  
- **Backtesting**: Replay historical data to backtest and optimise strategies.
- **Async/Tokio**: seemless integration, allows you to leverage async at your graph edges.
- **Multi-threading**: distribute graph execution across cores.

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
read the [documentation](https://docs.rs/wingfoil/latest/wingfoil/) 
or jump straight into [one the examples](https://github.com/wingfoil-io/wingfoil/tree/main/examples/order_book).

We want to hear from you!  Especially if you:
- are interested in contributing
- know of a project that wingfoil would be well suited for
- would like to request a feature
- have any feedback

Please email us at [hello@wingfoil.io](mailto:hello@wingfoil.io) or connect on [linkedin](https://www.linkedin.com/in/jake-mitchell-x77/).