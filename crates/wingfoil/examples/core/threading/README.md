## Threading

Multi-threaded graph execution: a **producer** sub-graph runs on its own worker
thread and feeds the **main** graph through the channel layer.

This is the wingfoil port of the legacy `threading` example. Legacy
provides `producer()` / `mapper()` combinators that run a sub-graph on a
dedicated thread and shuttle values over channels. Wingfoil deliberately keeps those
combinators out of the fluent vocabulary and instead exposes the primitive they
were built on directly (see the capability matrix in `docs/planning/port-plan.md` —
external / channel sources are `THREADED`, the sugar is not):

- **`GraphBuilder::channel()`** returns a source `Stream<Burst<T>>` plus a
  clonable, `Send` `ChannelSender<T>`.
- A worker thread builds and runs its **own** graph and forwards each value
  through the sender.
- The main graph receives a **burst** of everything that arrived since its last
  cycle — never latest-wins, never dropped.

Each graph stays single-threaded and lock-free; they touch only at the channel.
This mirrors wingfoil's rule of thumb: *no locks on the execution path — use the
channel layer to talk to background threads.* Additional stages chain the same
way: each is a worker thread whose graph has a channel *in* and a
`ChannelSender` *out*. Here the "mapper" stage (scale ×10) needs no thread of its
own, so it is just an op on the receiving main graph.

## Both run modes

- **Realtime** — the worker paces at its ticker `period`; whatever arrived since
  the last cycle rides one burst, so the grouping is wall-clock dependent.
- **Historical** — the worker sends timestamped values (`send_at`) and the
  receiver replays them **deterministically** at those graph times. Same
  topology, reproducible timeline: `[10]`, `[20]`, … `[60]`.

Each burst is printed from a `for_each` sink as the main graph receives it,
rather than accumulated into a `Vec` and dumped afterwards — a channel-fed graph
has no end to dump at.

## Running

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example threading
```

```
realtime  : [10]
realtime  : [20]
realtime  : [30]
realtime  : [40]
realtime  : [50]
realtime  : [60]
historical: [10]
historical: [20]
historical: [30]
historical: [40]
historical: [50]
historical: [60]
```

(Under load the realtime run may coalesce consecutive values into a single
burst, e.g. `realtime  : [10, 20]` — the historical run never varies.)
