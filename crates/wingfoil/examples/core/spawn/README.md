## Thread offload — `spawn` and `spawn_map`

Two combinators that move work onto worker threads:

- **`spawn`** runs a *producer* sub-graph on its own thread, feeding this graph.
- **`spawn_map`** maps an input stream *through* a sub-graph on another thread.

They are wingfoil's ergonomic twins of classic `producer()` / `mapper()`
(the `graph_node` node). The channel + `send_at` + `close` + join plumbing that
the [`threading`](../threading/) example spells out by hand is wrapped into one
call each — read `threading` first if you want to see what these hide.

```rust
let g = GraphBuilder::new();

// A producer sub-graph (ticker → running count) on its own worker thread.
// Values arrive as one `Burst` per instant.
let counts: Stream<Burst<u64>> = g.spawn(move |wg| wg.ticker(period).count().limit(n));
let flat:   Stream<u64>        = counts.map(|b: &Burst<u64>| b.iter().sum::<u64>());

// Map each value ×10 through a sub-graph on a *second* worker thread.
let scaled: Stream<Burst<u64>> = flat.spawn_map(|s: Stream<Burst<u64>>| {
    s.map(|b: &Burst<u64>| b.iter().sum::<u64>() * 10)
});
```

### Why `Burst<T>` on the way back

Anything crossing the channel layer arrives as a `Burst<T>` — every value that
landed at the same graph instant, together, never coalesced and never
latest-wins. A single-valued burst still arrives as a burst, which is why both
stages above `.iter().sum()` to flatten it. That uniformity is what lets
historical replay be exact: two same-instant values stay two values.

### Deterministic in historical mode

Both worker graphs run lock-step *by graph time*, not wall time. Under
`RunMode::HistoricalFrom` the run races to its bound with no sleeping and prints
identical numbers every time — threads do not make the result non-deterministic.

### Output

```text
  0 ms: [10]
 10 ms: [20]
 20 ms: [30]
 30 ms: [40]
 40 ms: [50]
```

### Run

```sh
cargo run -p wingfoil --example spawn
```

### Where to go next

- [`threading`](../threading/) — the same offload written out by hand.
- [`async`](../async/) — the other way off the graph thread, via tokio.
