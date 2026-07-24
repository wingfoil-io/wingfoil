## Run modes: realtime vs historical

The same graph runs two ways. `RunMode::RealTime` advances on the wall clock —
what a live deployment uses. `RunMode::HistoricalFrom(t)` replays on a simulated
clock as fast as the CPU allows — what backtests and deterministic tests use.

This is the fluent-API port of the classic `run_mode` example. A
`MarketDataBuilder` trait supplies the price stream; a realtime deployment and a
historical replay pick different implementations, but the business logic wired
on top of `price` is identical in both. Here both implementations share the
default mock so the two modes produce the same numbers.

```sh
cargo run -p wingfoil-next --example run_mode -- realtime
cargo run -p wingfoil-next --example run_mode -- historical
```

Both print the same last price (the mock cycles 100.0 – 109.0):

```text
last price: 105
```

### Idiom note

In the next engine sources are constructed *from a `GraphBuilder`*, so the
trait method takes `&GraphBuilder` and returns a `Stream<f64>` — whereas the
classic engine's `price()` took no arguments and returned an `Rc<dyn
Stream<f64>>`. The dispatch-on-run-mode pattern is otherwise unchanged.
