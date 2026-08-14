## Demux routing — a price book on a fixed slot pool

The **statically-wired** counterpart to the
[`dynamic_group`](../dynamic_group/) example. Both maintain the same live price
book — instruments come and go, and a `BTreeMap` of the latest price per live
instrument is kept correct — but this one never mutates the graph. A fixed pool
of slots is wired once, and `Builder::demux_it` routes each event to its
instrument's slot at runtime, recycling a slot when that instrument is deleted
(`DemuxEvent::Close`). Resource use tracks *concurrent* instruments, not
all-time.

Price and delete events share one `InstEvent` stream (merged with `combine`) so
simultaneous ticks ride a single burst; `demux_it` then routes each item
independently. The per-slot bursts are recombined, flattened, and folded into
the book.

```rust,ignore
let all_events = g.combine(&[price_events, del_events]).handle();

let (slots, overflow) = b.demux_it(all_events, CAPACITY, |event: &InstEvent| {
    let demux = match event {
        InstEvent::Delete(_) => DemuxEvent::Close,
        _ => DemuxEvent::None,
    };
    (inst_key(event), demux)
});
```

```text
price book (demux_it): {inst1=101}
price book (demux_it): {inst1=101, inst2=202}
price book (demux_it): {inst2=202, inst3=303}
price book (demux_it): {inst2=204, inst3=303}
price book (demux_it): {inst2=204, inst3=305}
price book (demux_it): {inst3=305, inst4=406}
price book (demux_it): {inst3=307, inst4=406}
price book (demux_it): {inst3=307, inst4=408}
price book (demux_it): {inst4=408, inst5=509}
price book (demux_it): {inst4=410, inst5=509}
price book (demux_it): {inst4=410, inst5=511}
price book (demux_it): {inst5=511, inst6=612}
price book (demux_it): {inst5=513, inst6=612}
price book (demux_it): {inst5=513, inst6=614}
price book (demux_it): {inst6=614, inst7=715}
price book (demux_it): {inst6=616, inst7=715}
price book (demux_it): {inst6=616, inst7=717}
price book (demux_it): {inst7=717, inst8=818}
price book (demux_it): {inst7=719, inst8=818}
price book (demux_it): {inst7=719, inst8=820}
```

Twenty states, one per cycle, matching legacy wingfoil's `demux` example exactly.
It diverges from the graph-mutating approaches only at the start, and for a real
reason: demux discovers instruments from *price* events rather than from an
explicit add stream, so on the third cycle — where the burst carries both
`Price(inst3, 303)` and `Delete(inst1)` — a slot opens for `inst3` immediately
and the book emits. From the sixth cycle on, the sequences agree.

No graph mutation means no `run_dynamic` and no splice cycles, so `RunFor::Cycles`
addresses ticker ticks directly.

The overflow child is wired too, not left dangling: choosing `CAPACITY` up front
is what fixed-topology routing costs, so an event that finds no free slot has to
go somewhere. Here it aborts the run —

```rust,ignore
let _overflowed = overflow.for_each(|events: &Burst<InstEvent>| {
    bail!("demux overflow: {CAPACITY} slots exhausted, unrouted {events:?}")
});
```

— which is the wingfoil spelling of legacy's `overflow.panic()`. The test
asserts the child never fires at all, since peak concurrency stays well under
ten.

No feature flag: demux mutates nothing, so it is not behind `dynamic-graph`
(legacy's `demux` example is ungated too).

```bash
cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux
```

`demux_it` is the only demux API that can route a price and a delete for
*different* instruments on the same cycle — see [`demux_map`](../demux_map/) for
the single-value form and what that costs, and `tests/dynamic_graph.rs` for the
full routing surface, including the raw `demux` primitive both are built on.
