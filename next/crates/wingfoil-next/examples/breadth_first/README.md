## Breadth-first graph execution

Wingfoil-next inherits the classic engine's breadth-first scheduler, which
eliminates the O(2^N) explosion that affects depth-first frameworks (reactive
libraries, async streams) when nodes branch and recombine.

Each `join(&source, &source)` branches the upstream node into two inputs and
recombines them. Depth-first frameworks visit every path through the graph —
2^N paths at depth N. Wingfoil's BFS scheduler visits each node exactly once
per tick, regardless of how many upstream paths lead to it.

```rust
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

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

This is the fluent-API port of the classic `breadth_first` example (which uses
`add(&source, &source)` over the classic engine). The next engine expresses the
self-referential diamond with `join`, whose two inputs are the same stream
handle.
