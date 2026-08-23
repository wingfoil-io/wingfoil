## Dynamic graphs — a live price book via `dynamic_group`

Wingfoil can add and remove nodes on a **running** graph, between engine
cycles, without stopping execution. This example maintains a live price book:
instruments are introduced and deleted at runtime, and a `BTreeMap` of the
latest price per live instrument is kept correct across every change.

The engine primitive is `Builder::dynamic_group` — the wingfoil twin of legacy
wingfoil's `dynamic_group_stream`. Give it an `add` stream, a `del` stream, and
a factory that builds a per-key sub-graph. On each `add` it splices the
sub-graph in; on each `del` it tears it down; every cycle it folds each live
member's *current* value into an aggregate you own.

```rust,ignore
b.dynamic_group(
    src.new_instrument.handle(),
    src.del_instrument.handle(),
    // Per-instrument sub-graph: the shared feed, filtered to this instrument.
    move |ext: &mut Extension<'_>, inst: Instrument| {
        let mine = ext.filter_value(feed, move |(i, _)| *i == inst);
        ext.map(mine, |(_, px)| round(*px))
    },
    PriceBook::new(),
    |book, inst, px| { book.insert(inst.clone(), *px); },
    |book, inst| { book.remove(inst); },
)
```

The scenario comes from the shared [`market_data.rs`](../market_data.rs): a
lifecycle ticker adds an instrument on two ticks out of three and deletes the
oldest on the third, while a price ticker sweeps a sliding window of ids.

```text
price book (dynamic_group): {inst1=101}
price book (dynamic_group): {inst1=101, inst2=202}
price book (dynamic_group): {inst2=204}
price book (dynamic_group): {inst2=204, inst3=305}
price book (dynamic_group): {inst3=305, inst4=406}
price book (dynamic_group): {inst3=307, inst4=406}
price book (dynamic_group): {inst3=307, inst4=408}
price book (dynamic_group): {inst4=408, inst5=509}
price book (dynamic_group): {inst4=410, inst5=509}
price book (dynamic_group): {inst4=410, inst5=511}
price book (dynamic_group): {inst5=511, inst6=612}
price book (dynamic_group): {inst5=513, inst6=612}
price book (dynamic_group): {inst5=513, inst6=614}
price book (dynamic_group): {inst6=614, inst7=715}
price book (dynamic_group): {inst6=616, inst7=715}
price book (dynamic_group): {inst6=616, inst7=717}
price book (dynamic_group): {inst7=717, inst8=818}
price book (dynamic_group): {inst7=719, inst8=818}
price book (dynamic_group): {inst7=719, inst8=820}
```

Nineteen states over twenty lifecycle ticks — the wiring changed underneath a
running graph, and this matches legacy's `dynamic-group` example state for
state. There is no emission on the third tick: only a deletion fires there, and
that cycle's price is for an instrument not yet live.

```bash
cargo run -p wingfoil --example dynamic --features dynamic-graph
```

Runtime graph mutation lives behind the `dynamic-graph` feature and on the
interpreted `GraphBuilder` / `Handle` layer (not the fluent `Stream` layer),
because it mutates topology mid-run. Because splicing a sub-graph in or out
consumes scheduler cycles, the run is bounded by `RunFor::Duration` (which maps
cleanly to ticker ticks) rather than `RunFor::Cycles`.

See [`dynamic_manual`](../dynamic_manual/) for the same splicing driven by hand,
the [`demux_*`](../) examples for the same book without touching the graph, and
`tests/dynamic_graph.rs` for the full primitive surface.
