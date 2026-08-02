## Dynamic graphs — a live price book

Wingfoil Next can add and remove nodes on a **running** graph, between engine
cycles, without stopping execution. This example maintains a live price book:
instruments are introduced and deleted at runtime, and a `BTreeMap` of the
latest price per live instrument is kept correct across every change.

The whole scenario is synthetic and deterministic — a per-cycle counter drives
a price feed, an *add* stream, and a *del* stream — so the run is reproducible
and the accompanying test asserts the exact book states the group emits.

```text
price book: {inst1=110}
price book: {inst1=110, inst2=220}
price book: {inst1=110, inst2=220, inst3=330}
price book: {inst3=360}
```

The book grows as `inst1`, `inst2`, `inst3` are added, then collapses to just
`inst3` once `inst1` and `inst2` are deleted — the wiring changed underneath a
running graph.

The engine primitive is [`Builder::dynamic_group`]: give it an `add` stream, a
`del` stream, and a factory that builds a per-key sub-graph. On each `add` it
splices the sub-graph in; on each `del` it tears it down; every cycle it folds
each live member's current value into an aggregate you own. This is the next
twin of legacy wingfoil's `dynamic_group_stream`.

```bash
cargo run -p wingfoil-next --example dynamic --features dynamic-graph
```

Runtime graph mutation lives behind the `dynamic-graph` feature and on the
interpreted `GraphBuilder` / `Handle` layer (not the fluent `Stream` layer),
because it mutates topology mid-run. Because splicing a sub-graph in or out
consumes scheduler cycles, the run is bounded by `RunFor::Duration` (which maps
cleanly to ticker ticks) rather than `RunFor::Cycles`. See
`tests/dynamic_graph.rs` for the full set of primitives (`dynamic_group`,
`demux`, `demux_it`, `demux_map`, and the low-level `add_upstream` /
`remove_node` driver hooks).

For the same price book built **without** mutating the graph — a fixed slot pool
routed by `demux_it` — see the [`demux`](../demux) example.
