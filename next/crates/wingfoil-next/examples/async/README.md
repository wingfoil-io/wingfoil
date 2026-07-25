## async / tokio integration

An async producer of **timestamped** values driving a wingfoil-next graph — the
classic `produce_async` model, ported to next.

Async streams are a natural fit for IO but an awkward one for business logic:
their execution is implicit and depth-first. Wingfoil's is explicit,
breadth-first and time-aware — with first-class historical *and* realtime
modes, so strategies backtest and run live off the same wiring. The
`produce_async` bridge keeps the best of both worlds: IO lives in the async
producer, business logic lives in the graph, and the boundary between them is a
single typed edge. That separation is exactly what tends to blur in
async-oriented systems.

The key call is **`produce_async`**, which maps an async `futures::Stream` of
`(NanoTime, T)` onto a graph source. The graph itself is the consumer: classic
hands the stream to an async `consume_async` closure, whereas on next an
on-graph `for_each` plays that role — keeping the consumer in the
explicitly-timed, breadth-first world. The producer runs on the caller's tokio
runtime and each yielded value wakes the kernel.

`produce_async` also carries the two guarantees classic gives for free:
**back-pressure** (`produce_async_bounded` bounds how far a realtime producer
may run ahead of the graph) and **`RunParams` validation** (a historical
`start_time` that disagrees with the actual run is rejected rather than
silently replaying against a bogus timeline). See the `async_source` module
docs for details, and `produce_async_feed` for the historical-replay variant.

## Running

Gated behind the `async` feature (tokio + futures):

```sh
RUST_LOG=info cargo run -p wingfoil-next --features async --example async
```

```
0
10
20
30
40
50
60
70
```

The producer awaits between yields (simulating socket reads), emits eight
timestamped values, then ends — closing the stream, which stops the graph.
