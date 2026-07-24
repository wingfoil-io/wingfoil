# Wingfoil Next

Wingfoil Next is the ground-up redesign of [wingfoil](../README.md)'s core:
node semantics are written **once**, as pure [`Op`] functions over
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

## Design objectives

1. **A strict superset of legacy wingfoil — including examples.** Before
   cutover, everything the legacy tree offers must exist here: every
   node/operator, every adapter, every run mode and execution pattern, the
   examples, benchmarks, language bindings and docs. Where next deliberately
   deviates (e.g. by-design `compiled()` restrictions), the deviation is
   documented in the capability matrix in [`docs/port-plan.md`](docs/port-plan.md)
   — never left implicit. Anything legacy does that next cannot do (or has
   not explicitly ruled out) is a cutover blocker.

2. **Ready to swap out the legacy tree wholesale.** This folder mirrors the
   legacy repo root — `README`, `LICENSE`, `CONTRIBUTING`, `docs/`, and the
   crates under `crates/` — so the eventual cutover is a directory promotion,
   not a re-organisation. Until then, the legacy crates keep shipping
   untouched and serve as the permanent parity oracle for the port.

3. **Single-sourced semantics, dual execution.** An op is *only* semantics:
   no storage (state is an associated type the engine owns), no upstream
   pointers (typed inputs are passed in per cycle), and a `const ACTIVATION`
   declaration instead of hidden scheduling behaviour. Both engines execute
   the identical monomorphizable `Op::cycle` functions.

4. **Lossless by default.** Same-instant values ride one `Burst` — never
   coalesced, never latest-wins — in realtime and in deterministic
   historical replay alike.

5. **Fallible everywhere.** Every lifecycle function (`start` / `cycle` /
   `stop` / `teardown`) returns `anyhow::Result`; errors abort the run with
   context, and cleanup still runs.

6. **An open vocabulary.** Sources, combinators, statistics and adapters are
   *extension traits* over two public primitives (`GraphBuilder::source`,
   `Stream::wire`) — third-party ops wire in exactly the way built-ins do,
   and `#[op(build = ...)]` gives user ops the same interpreted + compiled
   coverage with no macro table to edit.

## Layout

```
next/
  README.md               # This file
  LICENSE.txt             # Apache-2.0, same terms as the legacy tree
  CONTRIBUTING.md         # How to build, test, and port features across
  CLAUDE.md               # Guidance for Claude Code when working in next/
  docs/
    port-plan.md          # The port roadmap: phases, capability matrix, gates
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

# Examples (each ported legacy example keeps its name)
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

## Status

Porting is in progress, phase by phase, with the legacy test suite as the
parity oracle — see [`docs/port-plan.md`](docs/port-plan.md) for the live
✅/🟡/⬜ state. The port can pause at any phase boundary with everything
shipped still correct; the legacy crates remain the production engine until
the superset objective above is met.
