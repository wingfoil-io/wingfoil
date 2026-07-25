# Wingfoil Next

Wingfoil Next is a Rust stream-processing engine for building directed acyclic
graphs (DAGs) of data transformations, in realtime or in historical
(backtesting) replay.

Node semantics are written **once**, as pure [`Op`] functions over
engine-owned state, and executed by multiple engines — an interpreted engine
for flexibility, a fully monomorphized `compiled()` runner for speed, and
compiled islands (`nested()`) mounted as single nodes inside interpreted
graphs. All three are derived from the same wiring tokens by the `graph!`
macro, so the engines *cannot* drift: there is no duplicated cycle logic
anywhere.

```rust
use std::time::Duration;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

fn main() {
    let g = GraphBuilder::new();
    let msgs = g
        .ticker(Duration::from_millis(100))
        .count()
        .map(|i| format!("tick {i}"))
        .accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();
    for msg in runner.value(&msgs) {
        println!("{msg}");
    }
}
```

## Design principles

1. **Single-sourced semantics, dual execution.** An op is *only* semantics:
   no storage (state is an associated type the engine owns), no upstream
   pointers (typed inputs are passed in per cycle), and a `const ACTIVATION`
   declaration instead of hidden scheduling behaviour. Every engine executes
   the identical monomorphizable `Op::cycle` functions.

2. **Lossless by default.** Same-instant values ride one `Burst` — never
   coalesced, never latest-wins — in realtime and in deterministic
   historical replay alike.

3. **Fallible everywhere.** Every lifecycle function (`start` / `cycle` /
   `stop` / `teardown`) returns `anyhow::Result`; errors abort the run with
   context, and cleanup still runs.

4. **An open vocabulary.** Sources, combinators, statistics and adapters are
   *extension traits* over two public primitives (`GraphBuilder::source`,
   `Stream::wire`) — third-party ops wire in exactly the way built-ins do,
   and `#[op(build = ...)]` gives user ops the same interpreted + compiled
   coverage with no macro table to edit.

## Layout

```
next/
  README.md               # This file
  LICENSE.txt             # Apache-2.0
  CONTRIBUTING.md         # How to build, test, and contribute
  CLAUDE.md               # Guidance for Claude Code when working in next/
  docs/
    port-plan.md          # Phase-by-phase roadmap, capability matrix, gates
    cutover-plan.md       # Goals and status for replacing the legacy tree
    fable-review.md       # Design review of the plan + implementation
    macro-extensibility-decision.md  # Why graph! has no per-op table
  crates/
    wingfoil-next/        # The engine: Op trait, interpreted engine, ops,
                          #   stats, adapters, channel/async sources,
                          #   examples, tests, benches
    wingfoil-next-macros/ # graph! (one wiring fn -> interpreted/compiled/
                          #   nested) and #[op] proc macros
```

## Build and test

Run from the repository root — the crates are members of the root workspace:

```bash
# Build / test
cargo build -p wingfoil-next
cargo test  -p wingfoil-next
cargo test  -p wingfoil-next --all-features   # + async, csv, augurs adapters

# Examples
cargo run -p wingfoil-next --example hello_graph
cargo run -p wingfoil-next --example order_book
cargo run -p wingfoil-next --example csv_adapter --features csv

# Benchmarks (three-tier regression gate: interpreted vs compiled vs nested)
cargo bench -p wingfoil-next

# Lint (workspace-wide aliases, mirror CI)
cargo lint
cargo lint-all
cargo fmt --all -- --check
```

## Execution tiers

One wiring function, wrapped in `graph! { fn my_graph(g: &GraphBuilder) -> ... }`,
expands to a module offering all three:

| Tier | Entry point | What it is |
|---|---|---|
| Interpreted | fluent chaining directly, or `my_graph::interpreted()` | One dyn boundary per op; open world — threaded/busy-poll sources, feedback, bursts |
| Compiled | `my_graph::compiled(run_mode, run_for)` | The whole graph monomorphized into one function, state in locals — fastest, static DAGs only |
| Nested (island) | `my_graph::nested(&g, inputs...)` | A compiled sub-graph mounted as one node of an interpreted graph — hot core compiled, edges stay open |

See the capability matrix in [`docs/port-plan.md`](docs/port-plan.md) for
exactly what each tier supports and which restrictions are by design.
