# Docs

Three kinds of document live here, and the split is by audience.

## Start here

If you are reading one thing, read
[`wingfoil-architecture.md`](wingfoil-architecture.md) — the shape of the
engine, the one decision everything else follows from, and the rules that bite.

| | |
|---|---|
| [**wingfoil-architecture.md**](wingfoil-architecture.md) | The engine: `Op`, `Tick`, wiring, the two clocks, the three tiers |
| [**migration.md**](migration.md) | Porting Rust code off the 8.x `#[node]` engine onto `Op` |
| [**python-interop.md**](python-interop.md) | The plugin SDK — authoring ops, graphs and adapters in Rust, then composing them from Python |
| [**release-notes/**](release-notes/) | One page per version — what changed, why, and what you have to do about it |

## [`decisions/`](decisions/) — why the engine is the way it is

One page per ruling, written when the ruling was made. These explain *why*, and
they are the reason a later change does not quietly undo an earlier argument.
Read the relevant one before revisiting the ground it covers. Each carries its
own status — not all of them are built.

| | Status | |
|---|---|---|
| [`runtime-ownership.md`](decisions/runtime-ownership.md) | implemented | The graph owns the tokio runtime, with an override |
| [`source-lifecycle-defer-to-start.md`](decisions/source-lifecycle-defer-to-start.md) | implemented | Why sources establish their I/O in `start()`, not at construction |
| [`macro-extensibility-decision.md`](decisions/macro-extensibility-decision.md) | implemented | Why `nitro!` has no per-op table — `#[op(build = …)]` instead, so user ops take the built-in path |
| [`wired-graph-codegen-decision.md`](decisions/wired-graph-codegen-decision.md) | accepted, not built | Two-pass codegen from a wired graph — the `func!` quotation design |
| [`fpga-hdl-backend-decision.md`](decisions/fpga-hdl-backend-decision.md) | exploratory | FPGA/Verilog as a third backend, via RustHDL/RHDL emission |

## [`planning/`](planning/) — internal, and mostly historical

Working documents for the port and the cutover. They are kept because they are
the parity record, not because they are a backlog — **open work lives in GitHub
issues**, not here.

| | |
|---|---|
| [`port-plan.md`](planning/port-plan.md) | The historical record of the port, plus the capability matrix and the "Adding an op" touch-point table |
| [`cutover-plan.md`](planning/cutover-plan.md) | The plan for swapping the engines |
| [`cutover-runbook.md`](planning/cutover-runbook.md) | The step-by-step swap itself |
| [`deviation-register.md`](planning/deviation-register.md) | Every place wingfoil deliberately differs from legacy, classified |
| [`introspection-plan.md`](planning/introspection-plan.md) | Seeing the graph you wired — the structural snapshot has landed, the rest is scoped |
| [`trading-roadmap.md`](planning/trading-roadmap.md) | Wingfoil as an electronic trading platform — evaluation and phased plan (agreed direction, not a backlog) |
| [`comparison.md`](planning/comparison.md) | Wingfoil against other stream processing, dataflow and trading frameworks |

## Adding a page

Put it at the top level only if a *user* would look for it. A ruling that
closes an argument goes in `decisions/`; anything that is a working document
for us goes in `planning/`. Release notes get a page per version — see
[`release-notes/README.md`](release-notes/README.md).
