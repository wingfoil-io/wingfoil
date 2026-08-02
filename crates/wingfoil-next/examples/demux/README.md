## Demux routing — a price book on a fixed slot pool

This is the **statically-wired** counterpart to the [`dynamic`](../dynamic)
example. Both maintain the same live price book — instruments come and go, and a
`BTreeMap` of the latest price per live instrument is kept correct — but this
one never mutates the graph. A fixed pool of slots is wired once, and
[`Builder::demux_it`] routes each event to its instrument's slot at runtime,
recycling a slot when that instrument is deleted (`DemuxEvent::Close`). Resource
use tracks *concurrent* instruments, not all-time.

Price and delete events share one `InstEvent` stream (merged with `combine`) so
simultaneous ticks ride a single burst; `demux_it` then routes each item
independently. The per-slot bursts are recombined, flattened, and folded into
the book.

```text
price book: {inst1=110}
price book: {inst1=110, inst2=220}
price book: {inst1=110, inst2=220, inst3=330}
price book: {inst2=220, inst3=330}
price book: {inst3=330}
price book: {inst3=360}
```

The book grows as `inst1`, `inst2`, `inst3` are discovered from price events,
then shrinks as `inst1` and `inst2` are deleted — all on a fixed topology, no
`run_dynamic`.

```bash
cargo run -p wingfoil-next --example demux --features dynamic-graph
```

This is the next twin of classic wingfoil's `demux` example. See the
[`dynamic`](../dynamic) example for the graph-mutation approach to the same
problem, and `tests/dynamic_graph.rs` for the full routing surface (`demux`,
`demux_it`, `demux_map`).
