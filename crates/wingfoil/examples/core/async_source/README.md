## Async quote feed — `external` sources

An async market-data feed driving a wingfoil graph. A tokio task pushes
quotes into an `external` source; each send wakes the kernel. The graph maintains
a running mean and flags quotes that deviate from it by more than 1%.

The graph thread and the async world touch at exactly one place — the
`ExternalSource` handle. The graph itself stays single-threaded and lock-free.

```rust
let g = GraphBuilder::new();
let (quotes, feed) = g.external::<f64>();

// Fold over each burst: sum and count every quote it carries.
let mean = quotes
    .fold((0.0_f64, 0u64), |st, burst| {
        for q in burst.iter() { st.0 += *q; st.1 += 1; }
    })
    .map(|(sum, n)| if *n == 0 { 0.0 } else { sum / *n as f64 });

// The async producer, on its own thread with its own tokio runtime.
std::thread::spawn(move || {
    rt.block_on(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if !feed.send(next_price()) {
                break; // runner finished
            }
        }
    });
});
```

### Bursts, not latest-wins

`quotes` is a `Stream<Burst<f64>>`, not a `Stream<f64>`. The source emits **every
quote that arrived since the last cycle**, grouped — never coalesced, never
latest-wins, never dropped. If three quotes land between cycles you get a burst of
three, and the `fold` above counts all three toward the mean.

That is why the printed line reports `burst of N`: under a slow consumer or a
fast feed, N climbs, and nothing is silently lost. A latest-wins source would
quietly bias the mean toward whichever quote happened to arrive last.

### Backpressure and shutdown

`feed.send(..)` returns `false` once the runner is gone, which is how the producer
task learns to stop. There is no separate shutdown channel and no `Drop` ordering
to get right.

The report itself is a `for_each` sink, printing each line as its quote arrives.
A realtime feed has no end to dump a collected `Vec` at, so `accumulate()` here
would just be an unbounded buffer.

`external` is a **realtime-only** source: it is driven by wall-clock arrivals, so
there is nothing to replay deterministically. For an async producer that *does*
work in both modes, see [`async`](../async/), whose values carry their own
timestamps.

### Output

```text
quotes from the async feed:
  burst of  1  last   99.98  mean   99.98  dev +0.00%
  burst of  1  last   97.71  mean   98.85  dev -1.15%  <-- outlier
  burst of  1  last   97.94  mean   98.55  dev -0.61%
  burst of  1  last   98.01  mean   98.41  dev -0.41%
  ...
```

### Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example async_source --features async
```

### Where to go next

- [`async`](../async/) — `produce_async`: timestamped async values, both modes.
- [`spawn`](../spawn/) — offloading onto threads rather than tokio.
