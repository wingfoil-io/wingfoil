## Topologically sorted graph execution

Wingfoil-next inherits the legacy engine's topologically sorted scheduler,
which eliminates the O(2^N) explosion that affects frameworks propagating one
path at a time (reactive libraries, async streams) when nodes branch and
recombine.

Each `join(&source, &source)` branches the upstream node into two inputs and
recombines them. A path-at-a-time framework walks every path through the
graph — 2^N paths at depth N. Wingfoil sorts the graph topologically and
visits each node exactly once per tick, after everything it reads, regardless
of how many upstream paths lead to it.

```rust
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil::prelude::*;

let g = GraphBuilder::new();
let mut source = g.constant(1_u128);
for _ in 1..128 {
    source = source.join(&source, |a, b| a + b);
}
let out = source.timed();
let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
println!("value {:?}", runner.value(&out));
```

127 levels deep — 2^127 as the correct answer — completes in **one tick**:

```text
value 170141183460469231731687303715884105728
```

This is the fluent-API port of the legacy `breadth_first` example (which uses
`add(&source, &source)` over the legacy engine). The next engine expresses the
self-referential diamond with `join`, whose two inputs are the same stream
handle. The target keeps its historical name, so it still runs with
`cargo run --example breadth_first`.
