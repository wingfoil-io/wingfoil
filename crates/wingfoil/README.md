# wingfoil

The Wingfoil engine: a dual-mode stream-processing library for building
DAGs of data transformations, for latency-critical use cases such as electronic
trading and real-time AI systems.

You describe your graph once, in a single fluent wiring, and choose how to run
it. Every tier is derived from the same definition, so they cannot drift — there
is no duplicated execution logic anywhere.

```rust
use std::time::Duration;
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

GraphBuilder::new()
    .ticker(Duration::from_secs(1))
    .count()
    .map(|i| format!("hello, world {i}"))
    .print()
    .build()
    .run(RunMode::RealTime, RunFor::Cycles(3))
    .unwrap();
```

`build()` sits on `Stream<T>` as well as on `GraphBuilder`, so a whole program
can be one chain; it builds the graph either way. Hold the builder and the
streams in bindings when you branch or want to read values back after the run.

## Execution tiers

One wiring function wrapped in `nitro! { fn my_graph(g: &GraphBuilder) -> ... }`
expands to a module offering all three:

| Tier | Entry point | What it is |
|---|---|---|
| Interpreted | fluent chaining directly, or `my_graph::interpreted()` | One dyn boundary per op; open world — threaded/busy-poll sources, feedback, bursts. |
| Compiled | `my_graph::compiled(run_mode, run_for)` | The whole graph monomorphized into one function, state in locals — fastest, static DAGs. |
| Nested (island) | `my_graph::nested(&g, inputs...)` | A compiled sub-graph mounted as one node of an interpreted graph — hot core compiled, edges stay open. |

See [`examples/core/dual_mode/`](examples/core/dual_mode/) for what `nitro!`
accepts and why.

## Layout

| Path | What's in it |
|---|---|
| [`src/op.rs`](src/op.rs), [`src/interp.rs`](src/interp.rs) | The `Op` trait and the interpreter — the engine proper. |
| [`src/fluent.rs`](src/fluent.rs) | `GraphBuilder` and `Stream<T>`; the wiring layer. |
| [`src/ops.rs`](src/ops.rs), [`src/stats.rs`](src/stats.rs) | The op catalog and the statistics ops. |
| [`src/adapters/`](src/adapters/) | The I/O adapters, one directory each, all feature-gated. |
| [`src/runtime/`](src/runtime/) | The shared runtime core — engine time, run bounds, the time queue, `Burst`, the `Kernel`, the latency data layer. Re-exported by the legacy `wingfoil` crate. |
| [`src/channel.rs`](src/channel.rs), [`src/async_source.rs`](src/async_source.rs) | Thread and tokio edges. |
| [`src/signal.rs`](src/signal.rs) | The builder-less `Signal` facade. |
| [`examples/`](examples/) | ~40 runnable examples, grouped into `core/`, `adapters/`, `showcase/`. |
| [`benches/`](benches/) | Criterion benchmarks, including the three-tier comparison — with [captured readings and charts](benches/README.md). |

## Key concepts

- **`Op` trait** — semantics as associated *functions*, `cycle(cfg, state, input,
  ctx)`, never methods on an instantiated object. `Cfg` is construction-time
  config (closures live here), `State` is engine-owned mutable state, `In<'a>` is
  typed inputs passed per cycle, `Out` is the produced value. `const ACTIVATION`
  declares scheduling behaviour statically.
- **`Tick<T>`** — `Value` (tick downstream), `Silent` (update the value slot
  without ticking, what `delay` needs), `Quiet` (nothing).
- **Wiring** — combinators are *extension traits* (`SourceOps`, `StreamOps`,
  `StatisticsOps`, adapter traits). New vocabulary is added through the two public
  primitives `GraphBuilder::source` and `Stream::wire`, never by editing `Stream`.
- **Bursts** — same-instant values ride a single burst: never coalesced, never
  latest-wins, identically in realtime and historical replay.
- **Fallibility** — every lifecycle function returns `anyhow::Result`;
  `sender.send_error(e)` propagates a producer error into the graph and aborts the
  run with context, with cleanup still running.

## Features

Adapters and optional machinery are all feature-gated; nothing you don't ask for
is compiled. Adapter and stats ops stay **out of the prelude** — opt in with
`use wingfoil::adapters::<name>::...;`.

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example csv_adapter --features csv
```

See [`examples/adapters/README.md`](examples/adapters/README.md) for the feature
each adapter needs.

## Examples

Start with [`examples/core/hello_graph/`](examples/core/hello_graph/), then
[`ema_crossover`](examples/core/ema_crossover/) and
[`order_book`](examples/core/order_book/). The full index is
[`examples/README.md`](examples/README.md).

## Testing

Tests use `RunMode::HistoricalFrom(NanoTime::ZERO)` for determinism, and assert
exact values *and* tick times (`with_time()` + `accumulate()`).

```sh
cargo test --manifest-path crates/wingfoil/Cargo.toml
```

## See also

- [`../README.md`](../README.md) — the crate map for `crates/`
- [`../../README.md`](../../README.md) — Wingfoil overview
- [`../../docs/port-plan.md`](../../docs/port-plan.md) — the port roadmap and capability matrix
- [`CLAUDE.md`](../../CLAUDE.md) — working conventions for this tree
