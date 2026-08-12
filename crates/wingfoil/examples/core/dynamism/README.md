# Graph dynamism — one price book, four wirings

Instruments are created and deleted while the engine runs, and a `BTreeMap` of
the latest price per live instrument has to stay correct across every change.
That is the whole problem, and these four examples each solve it a different
way over the **same** market-data scenario — the wingfoil twin of legacy
wingfoil's [`examples/dynamic/`](../../../../../legacy/wingfoil/examples/dynamic/).

Two mutate the running graph; the other two route over a topology that never
changes:

| Example | Primitive | Approach |
|---|---|---|
| [`dynamic_group`](dynamic_group/) | [`Builder::dynamic_group`] | High-level: hand it an `add` stream, a `del` stream and a per-key factory. |
| [`dynamic_manual`](dynamic_manual/) | [`Extension::add_upstream`] / [`Extension::remove`] | The same splicing driven by hand from the `run_dynamic` hook. |
| [`demux_it`](demux_it/) | [`Builder::demux_it`] | Fixed slot pool; routes **each item of a burst** to its instrument's slot. |
| [`demux_map`](demux_map/) | [`Builder::demux_map`] | Same pool and key lifecycle, but **one value per cycle**. |

```bash
cargo run --manifest-path crates/wingfoil/Cargo.toml --example dynamic        --features dynamic-graph
cargo run --manifest-path crates/wingfoil/Cargo.toml --example dynamic_manual --features dynamic-graph
cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux          --features dynamic-graph
cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux_map      --features dynamic-graph
```

(`dynamic_group` and `demux_it` keep legacy's target names — `dynamic`, `demux`
— so `cargo run --example …` still works from muscle memory.)

## The shared scenario

[`market_data.rs`](market_data.rs) is the source module all five pull in with
`#[path = "../market_data.rs"]`, exactly as the legacy tree does. Two tickers at
the same period drive it:

- **lifecycle** — every 3rd tick deletes the oldest live instrument, every other
  tick adds a fresh one (`inst1`, `inst2`, …);
- **prices** — one `(instrument, price)` per tick, the id sweeping a sliding
  window `id = (n-1)/3 + (n-1)%3 + 1` and the price climbing so every update is
  visible.

[`oracle.rs`](oracle.rs) holds the expected book states once, and every example's
test asserts against it — so a change to the scenario cannot quietly shift five
sets of expectations at the same time.

## Which one to reach for

**Graph mutation** (`dynamic_group`, `dynamic_manual`) costs nothing when
membership is idle and scales with the number of *live* keys — but splicing
consumes scheduler cycles, so a `RunFor::Cycles(n)` bound no longer maps onto
`n` ticker ticks (these two are bounded by `RunFor::Duration` for that reason),
and it needs `run_dynamic` rather than `run`.

**Demux routing** (`demux_it`, `demux_map`) keeps the topology fixed: a pool of
slots is wired once, and a slot is recycled (`DemuxEvent::Close`) when its key
retires, so resource use tracks *concurrent* keys rather than all-time. Nothing
is spliced, so `run` and `RunFor::Cycles` behave normally — at the cost of
choosing a capacity up front and wiring an overflow path for when it is
exceeded.

## One deviation, and why

`demux_it` routes **each item** of a burst independently, so a price for one
instrument and a delete for another can share a cycle — which is what the legacy
scenario does on every 3rd tick. `demux_map` — and the raw [`Builder::demux`]
primitive beneath it — routes exactly **one value per cycle** and cannot express
that.

So `demux_map` runs against `market_data_offset`, which phase-shifts the
lifecycle ticker by half a period. No price and no delete is lost — only *when*
they land changes — but the book then emits at 26 instants rather than 20, because each
delete gets its own cycle instead of sharing one with a price. `oracle.rs`
spells the relationship out state by state.

[`Builder::demux`]: https://docs.rs/wingfoil/latest/wingfoil/interp/struct.Builder.html#method.demux
[`Builder::demux_it`]: https://docs.rs/wingfoil/latest/wingfoil/interp/struct.Builder.html#method.demux_it
[`Builder::demux_map`]: https://docs.rs/wingfoil/latest/wingfoil/interp/struct.Builder.html#method.demux_map
[`Builder::dynamic_group`]: https://docs.rs/wingfoil/latest/wingfoil/interp/struct.Builder.html#method.dynamic_group
[`Extension::add_upstream`]: https://docs.rs/wingfoil/latest/wingfoil/interp/struct.Extension.html#method.add_upstream
[`Extension::remove`]: https://docs.rs/wingfoil/latest/wingfoil/interp/struct.Extension.html#method.remove
