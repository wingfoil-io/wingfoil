
# Wingfoil

Wingfoil is a [blazingly fast](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/), highly scalable 
stream processing framework designed for latency-critical use cases such as electronic trading 
and real-time AI systems.

Wingfoil simplifies receiving, processing and distributing streaming data across your entire stack.

Checkout the [wingfoil project page](https://github.com/wingfoil-io/wingfoil/) for more information.

## Features

- **Fast**: Ultra-low latency and high throughput with a efficent [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph) based execution engine.  
- **Simple and obvious to use**: Define your graph of calculations; Wingfoil manages its execution.  
- **Multi-language**: currently available as rust crate and a python package with plans to add WASM/JavaSript/TypeScript support.
- **Backtesting**: Replay historical data to backtest and optimise strategies.
- **Multi-threading**: distribute graph execution across cores.

## Installation (coming soon)

```bash
pip install wingfoil
```

## Quick Start

This python code...
```python
#!/usr/bin/env python3

from wingfoil import ticker

period = 1.0 # seconds
duration = 4.0 # seconds
stream = (
    ticker(period)
    .count()
    .logged("hello, world")
)

stream.run(realtime=True, duration=duration)

```
...produces this output...
```console
[2025-11-02T18:42:18Z INFO  wingfoil] 0.000_092 hello, world 1
[2025-11-02T18:42:19Z INFO  wingfoil] 1.008_038 hello, world 2
[2025-11-02T18:42:20Z INFO  wingfoil] 2.012_219 hello, world 3
```

You can follow these instructions to [build from source](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil-python/build.md).

We want to hear from you!  Especially if you:
- are interested in contributing
- know of a project that wingfoil would be well-suited for
- would like to request a feature
- Have any feedback

Please email us at [hello@wingfoil.io](mailto:hello@wingfoil.io) or get involved in the [wingfoil discussion](https://github.com/wingfoil-io/wingfoil/discussions/).  Take a look at the [issues](https://github.com/wingfoil-io/wingfoil/issues) for ideas on ways to contribute.
