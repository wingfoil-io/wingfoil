## Hello, graph — the smallest wingfoil program

The place to start. A ticker is counted and formatted into a string, and the
same three-node wiring is run twice: once in **historical** mode (instant,
deterministic) and once in **realtime** (the kernel waits out each tick on the
wall clock).

That pairing is the point. The wiring never mentions time-of-day — the run mode
does — so a graph you backtest and a graph you deploy are the same graph.

```rust
use std::time::Duration;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

let g = GraphBuilder::new();
let _printed = g
    .ticker(Duration::from_millis(100))
    .count()
    .map(|i| format!("tick {i}"))
    .for_each(|msg: &String| {
        println!("  {msg}");
        Ok(())
    });

let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))?;
```

Three things to notice:

- **`GraphBuilder` wires, `build()` freezes, `run()` executes.** You never hold a
  node object — `Stream<T>` is a handle, and a scalar like the realtime tick
  count comes back through `runner.value(&handle)` after the run.
- **`for_each()` is the graph's outbound edge** — a side-effecting sink that
  runs once per tick, so output streams out *as the run progresses*. `print()`
  is the one-call debug version (`{value:?}` per line), and `logged(..)` routes
  through the `log` crate. Reach for `accumulate()` only in tests, where a
  bounded run's whole sequence has to be asserted on afterwards; as an output
  edge it grows a `Vec` for the entire run.
- **`RunFor::Cycles(5)`** bounds the run. `RunFor::Duration(..)` and
  `RunFor::Forever` are the other two.

### Output

```text
historical run (instant):
  tick 1
  tick 2
  tick 3
  tick 4
  tick 5
realtime run (3 ticks, 50ms apart):
  counted 3 ticks
```

The historical block returns immediately — 5 ticks of a 100 ms ticker are
simulated, not waited for. The realtime block genuinely takes 150 ms.

### Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example hello_graph
```

### Where to go next

- [`ema_crossover`](../ema_crossover/) — the same primitives at realistic scale.
- [`run_mode`](../run_mode/) — the realtime/historical swap on its own.
- [`order_book`](../order_book/) — real stateful work in `fold`.
