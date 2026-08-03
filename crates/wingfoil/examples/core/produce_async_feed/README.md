## `produce_async` — timestamped async values, both modes

An async producer that yields **timestamped** values, driving a graph the same
way in realtime *and* historical replay.

This is the difference from [`async_source`](../async_source/). An `external`
source is driven by wall-clock arrivals, so it only makes sense in realtime.
`produce_async` yields `(NanoTime, T)` pairs — the value carries its own event
time — so the same producer can be replayed deterministically on the graph clock.
Record a feed once, replay it in a backtest, get the same answer every run.

```rust
// The graph owns the tokio runtime (created lazily); no `&Handle` to pass.
let g = GraphBuilder::new();

let quotes = produce_async(
    &g,
    |_p| async {
        Ok(futures::stream::unfold((0u32, 100.0_f64), |(i, price)| async move {
            if i >= 8 { return None; }               // a finite feed
            tokio::time::sleep(Duration::from_millis(1)).await;  // await, as a socket read would
            let price = price + (i as f64 % 3.0) - 1.0;
            let t = NanoTime::new(100 * (i as u64 + 1));         // the event's own timestamp
            Some((Ok((t, price)), (i + 1, price)))
        }))
    },
    None,
)?;

let mean = quotes
    .collapse_accumulate()
    .map(|qs| qs.iter().sum::<f64>() / qs.len().max(1) as f64);
```

### Notes on the shape

- **The graph owns the runtime.** It is created lazily on first use, so nothing
  threads a `&Handle` through your wiring. Pass one explicitly with
  `GraphBuilder::new().with_async_runtime(handle)` only when you need to control
  the runtime's lifetime — see the [`etcd`](../../adapters/etcd/) example.
- **The producer is fallible.** It yields `Result` items; an `Err` propagates into
  the graph and aborts the run with context, rather than being swallowed.
- **A finite feed closes.** Returning `None` ends the stream, which is why this
  can run with `RunFor::Forever` and still terminate.
- **`collapse_accumulate()`** flattens the bursts and accumulates in one step —
  the burst-aware counterpart to `accumulate()`.

### Output

```text
running mean of async feed: 99.250
```

Deterministic: the timestamps come from the producer, not the clock, so this
prints the same number on every run.

### Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example produce_async_feed --features async
```

### Where to go next

- [`async_source`](../async_source/) — realtime `external` sources.
- [`async`](../async/) — the classic `async` example ported.
- [`adapters/csv`](../../adapters/csv/) — the same replay machinery behind a real file source.
