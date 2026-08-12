## Single-value demux — `demux_map`, one routed value per cycle

The same price book as [`demux_it`](../demux_it/), on the same fixed slot pool
and the same `DemuxEvent` key lifecycle, but through `Builder::demux_map`: it
routes **one value per cycle** to **one** child, rather than fanning a burst's
items out across several.

That difference is the whole lesson. In the shared scenario a price for one
instrument and a delete for *another* land on the same tick every third cycle —
`demux_it` handles that natively, `demux_map` cannot express it. So this example
runs against `market_data_offset`, which phase-shifts the lifecycle ticker by
half a period so at most one event lands per cycle. Nothing is lost: every price
and every delete still arrives, just on its own cycle.

```rust,ignore
// Offset feed ⇒ the combined burst never holds more than one item, so it can
// be collapsed back to a single value for the single-value demux.
let events = g.combine(&[price_events, del_events]).collapse().handle();

let (slots, overflow) = b.demux_map(events, CAPACITY, |event: &InstEvent| {
    let demux = match event {
        InstEvent::Delete(_) => DemuxEvent::Close,
        _ => DemuxEvent::None,
    };
    (inst_key(event), demux)
});
```

Tick times are printed alongside the book, because they are what makes the
interleave visible — prices on the second, deletes on the half second:

```text
t=  0.0s  price book (demux_map): {inst1=101}
t=  1.0s  price book (demux_map): {inst1=101, inst2=202}
t=  2.0s  price book (demux_map): {inst1=101, inst2=202, inst3=303}
t=  2.5s  price book (demux_map): {inst2=202, inst3=303}
t=  3.0s  price book (demux_map): {inst2=204, inst3=303}
t=  4.0s  price book (demux_map): {inst2=204, inst3=305}
t=  5.0s  price book (demux_map): {inst2=204, inst3=305, inst4=406}
t=  5.5s  price book (demux_map): {inst3=305, inst4=406}
t=  6.0s  price book (demux_map): {inst3=307, inst4=406}
t=  7.0s  price book (demux_map): {inst3=307, inst4=408}
t=  8.0s  price book (demux_map): {inst3=307, inst4=408, inst5=509}
t=  8.5s  price book (demux_map): {inst4=408, inst5=509}
t=  9.0s  price book (demux_map): {inst4=410, inst5=509}
t= 10.0s  price book (demux_map): {inst4=410, inst5=511}
t= 11.0s  price book (demux_map): {inst4=410, inst5=511, inst6=612}
t= 11.5s  price book (demux_map): {inst5=511, inst6=612}
t= 12.0s  price book (demux_map): {inst5=513, inst6=612}
t= 13.0s  price book (demux_map): {inst5=513, inst6=614}
t= 14.0s  price book (demux_map): {inst5=513, inst6=614, inst7=715}
t= 14.5s  price book (demux_map): {inst6=614, inst7=715}
t= 15.0s  price book (demux_map): {inst6=616, inst7=715}
t= 16.0s  price book (demux_map): {inst6=616, inst7=717}
t= 17.0s  price book (demux_map): {inst6=616, inst7=717, inst8=818}
t= 17.5s  price book (demux_map): {inst7=717, inst8=818}
t= 18.0s  price book (demux_map): {inst7=719, inst8=818}
t= 19.0s  price book (demux_map): {inst7=719, inst8=820}
```

Twenty-six states rather than twenty: each of the six deletes now gets its own
emission instead of sharing a cycle with a price. Compare the half-second rows
against `demux_it`'s output and they are the same books — the extra rows are the
pre-delete states in between.

```bash
cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux_map --features dynamic-graph
```

So: **reach for `demux_map` when each cycle carries exactly one keyed value**
(a single order update, one message off a socket) — the wiring is simpler and
there is no burst to flatten afterwards. When events for different keys can
coincide, `demux_it` is the one that fits. Both are built on the raw `demux`
primitive, which routes by index and leaves the key→slot pool to you;
`tests/dynamic_graph.rs` exercises it directly.
