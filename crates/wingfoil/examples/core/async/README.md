## async / tokio integration

An async producer of **timestamped** values driving a wingfoil graph — the
legacy `produce_async` model, ported to wingfoil and run in **both** modes off
one definition.

Async streams are a natural fit for IO but an awkward one for business logic:
their execution is implicit and path-at-a-time. Wingfoil's is explicit,
topologically sorted and time-aware — with first-class historical *and* realtime
modes, so strategies backtest and run live off the same wiring. The
`produce_async` bridge keeps the best of both worlds: IO lives in the async
producer, business logic lives in the graph, and the boundary between them is a
single typed edge. That separation is exactly what tends to blur in
async-oriented systems.

The key call is **`produce_async`**, which maps an async `futures::Stream` of
`(NanoTime, T)` onto a graph source. The graph itself is the consumer: legacy
hands the stream to an async `consume_async` closure, whereas on wingfoil an
on-graph `for_each` plays that role — keeping the consumer in the
explicitly-timed, topologically sorted world. The producer runs on the graph's
own tokio runtime (created lazily) and each yielded value wakes the kernel.

### One producer, both modes

Each value carries its **own** event time, which is what lets the same producer
serve a live feed and a recorded one. The closure is handed the run's
`RunParams` — derived from the `run()` the graph actually started, not declared
up front — so it can decide where those timestamps come from:

```rust
let quotes = produce_async(
    &g,
    move |params: RunParams| async move {
        let historical = matches!(params.run_mode, RunMode::HistoricalFrom(_));
        Ok(futures::stream::unfold((0u32, 100.0_f64), move |(i, price)| async move {
            if i >= N { return None; }              // a finite feed, which closes
            tokio::time::sleep(IO_DELAY).await;     // await, as a socket read would
            let price = price + (i as f64 % 3.0) - 1.0;
            let time = if historical {
                params.start_time + PERIOD * i      // the event's own recorded time
            } else {
                NanoTime::now()                     // it ticks on arrival
            };
            Some((Ok((time, price)), (i + 1, price)))
        }))
    },
    None,
)?;
```

That is the difference from [`async_source`](../async_source/). An `external`
source is driven by wall-clock arrivals, so it only makes sense in realtime.
`produce_async` yields `(NanoTime, T)` pairs, so the same producer can be
replayed deterministically on the graph clock. Record a feed once, replay it in
a backtest, get the same answer every run.

Note that `IO_DELAY` (how long the producer awaits) and `PERIOD` (the spacing of
the recorded event times) are independent. In a historical run the quotes land
10 ms apart in *graph* time however fast the producer happens to yield them — a
backtest is not paced by how quickly the file reads.

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
  the burst-aware counterpart to `accumulate()`, so nothing is lost when a
  realtime cycle carries several quotes at once.
- **Two guarantees inherited from legacy:** back-pressure (`produce_async`
  bounds how far a producer may run ahead of the graph) and `RunParams`
  validation (a historical `start_time` that disagrees with the actual run is
  rejected rather than silently replaying against a bogus timeline). See the
  `async_source` module docs.

## Running

Gated behind the `async` feature (tokio + futures):

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --features async --example async
```

```text
historical replay (event times from the feed):
  +  0.0 ms   99.00
  + 10.0 ms   99.00
  + 20.0 ms  100.00
  + 30.0 ms   99.00
  + 40.0 ms   99.00
  + 50.0 ms  100.00
  + 60.0 ms   99.00
  + 70.0 ms   99.00
  mean 99.250

realtime (graph times are arrival times):
  +  0.0 ms   99.00
  +  2.2 ms   99.00
  +  4.3 ms  100.00
  +  6.4 ms   99.00
  +  8.6 ms   99.00
  + 10.7 ms  100.00
  + 13.0 ms   99.00
  + 15.1 ms   99.00
  mean 99.250
```

Same quotes both times — the example asserts it — and the same mean. Only the
tick times differ: the historical column is exact and reproduces on every run,
while the realtime one is whatever the wall clock said when each value landed
and will differ slightly for you.

### Where to go next

- [`async_source`](../async_source/) — realtime `external` sources, burst-delivered.
- [`adapters/csv`](../../adapters/csv/) — the same replay machinery behind a real file source.
- [`run_mode`](../run_mode/) — swapping the mode under a whole wiring.
