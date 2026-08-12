# Docs

Three kinds of document live here, and the split is by audience.

## Start here

If you are reading one thing, read
[`wingfoil-architecture.md`](wingfoil-architecture.md) — the shape of the
engine, the one decision everything else follows from, and the rules that bite.

| | |
|---|---|
| [**wingfoil-architecture.md**](wingfoil-architecture.md) | The engine: `Op`, `Tick`, wiring, the two clocks, the three tiers |
| [**adding-an-op.md**](adding-an-op.md) | The recipe and touch-point table for a new op — what `#[op]` generates, and why the compiled path is zero-touch |
| [**migration.md**](migration.md) | Porting Rust code off the 8.x `#[node]` engine onto `Op` |
| [**python-interop.md**](python-interop.md) | The plugin SDK — authoring ops, graphs and adapters in Rust, then composing them from Python |
| [**comparison.md**](comparison.md) | Wingfoil against other stream processing, dataflow and trading frameworks |
| [**release-notes/**](release-notes/) | One page per version — what changed, why, and what you have to do about it |

## [`decisions/`](decisions/) — why the engine is the way it is

One page per ruling, written when the ruling was made. These explain *why*, and
they are the reason a later change does not quietly undo an earlier argument.
Read the relevant one before revisiting the ground it covers.

**Everything in here is settled and true of `main`** — that is the entry
criterion, and "Ruling or record?" below is the test.

| | |
|---|---|
| [`runtime-ownership.md`](decisions/runtime-ownership.md) | The graph owns the tokio runtime, with an override |
| [`source-lifecycle.md`](decisions/source-lifecycle.md) | Why sources establish their I/O in `start()`, not at construction — and why the re-run half was dropped |
| [`macro-extensibility-decision.md`](decisions/macro-extensibility-decision.md) | Why `nitro!` has no per-op table — `#[op(build = …)]` instead, so user ops take the built-in path |

## [`planning/`](planning/) — internal, and mostly historical

Working documents for the port and the cutover. They are kept because they are
the parity record, not because they are a backlog — **open work lives in GitHub
issues**, not here.

| | |
|---|---|
| [`port-plan.md`](planning/port-plan.md) | The historical record of the port, plus the capability matrix |
| [`cutover-plan.md`](planning/cutover-plan.md) | The plan for swapping the engines |
| [`cutover-runbook.md`](planning/cutover-runbook.md) | The step-by-step swap itself |
| [`deviation-register.md`](planning/deviation-register.md) | Every place wingfoil deliberately differs from legacy, classified |
| [`introspection-plan.md`](planning/introspection-plan.md) | Seeing the graph you wired — the structural snapshot has landed, the rest is scoped |
| [`trading-roadmap.md`](planning/trading-roadmap.md) | Wingfoil as an electronic trading platform — evaluation and phased plan (agreed direction, not a backlog) |

### [`planning/proposals/`](planning/proposals/) — designed, argued, not built

Design bodies for work that has been thought through but not shipped. Each has
an open tracking issue carrying its sequencing as a checklist — **the issue is
the status, this is the reasoning.**

| | Status | |
|---|---|---|
| [`wired-graph-codegen.md`](planning/proposals/wired-graph-codegen.md) | accepted; implemented on the **unmerged** [#769](https://github.com/wingfoil-io/wingfoil/pull/769) branch, not on `main` | Two-pass codegen from a wired graph — the `func!` quotation design ([#726](https://github.com/wingfoil-io/wingfoil/issues/726)) |
| [`fpga-hdl-backend.md`](planning/proposals/fpga-hdl-backend.md) | exploratory, not scheduled | FPGA/Verilog as a third backend, via RustHDL/RHDL emission ([#727](https://github.com/wingfoil-io/wingfoil/issues/727)) |

## Adding a page

Put it at the top level only if a *user* would look for it. Release notes get a
page per version — see [`release-notes/README.md`](release-notes/README.md).
For everything else:

### Ruling or record?

The audience split above is the intent; this is the test that actually
discriminates, because both directories contain "why" prose and both contain
history.

| | `decisions/` | `planning/` |
|---|---|---|
| Answers | a question, once | tracks a body of work over time |
| Lifecycle | written when the ruling is made, then frozen | edited continuously, ticked off, goes stale |
| The test | *if the work never happens, is the doc still valuable?* → **yes**, the reasoning survives | → **no**, it is a stale backlog |
| Dies when | never | the work lands |

[`runtime-ownership.md`](decisions/runtime-ownership.md) is the model for a
decision record: question → decision → rationale → what shipped, in 70 lines.

Two mechanical consequences, both learned the hard way:

- **A "Sequencing" section plus an open tracking issue means it is a plan**,
  however good the argument in it. That is what moved the FPGA and codegen
  designs into `planning/proposals/`: a decision record whose own status reads
  *not scheduled* or *not built* is a category error, and both were being
  rewritten as the facts moved — which is what plans do and rulings do not.
- **A decision record must not carry open engineering work.** Nobody reading
  the tracker will find it there. File the issue and link it;
  `macro-extensibility-decision.md` §4 is the worked example.
