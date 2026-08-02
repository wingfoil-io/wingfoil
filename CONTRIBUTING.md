# Contributing to Wingfoil Next

Wingfoil Next is the ground-up rebuild of [wingfoil](../CONTRIBUTING.md) on
the Op pattern — see [`README.md`](README.md) for the design objectives and
[`docs/port-plan.md`](docs/port-plan.md) for the roadmap. Community channels,
licensing and general contribution etiquette are shared with the main
project: see the [top-level CONTRIBUTING](../CONTRIBUTING.md).

## What contributions look like here

The port advances phase by phase (see the plan's ✅/🟡/⬜ markers). The most
valuable contributions are:

- **Porting a legacy node/operator** — follow "Adding an op" in
  [`docs/port-plan.md`](docs/port-plan.md). Most single-input ops need only
  an `Op` impl with `#[op(build = ...)]` plus a 3-line fluent method; the
  compiled path is zero-touch.
- **Porting a legacy adapter** — follow the `/new-adapter-next` skill
  (`.claude/commands/new-adapter-next.md` from the repo root), which encodes
  the layering rules (sources over `channel`/`poll`, sinks over `for_each`,
  extension traits out of the prelude).
- **Porting a legacy example or test** — every classic example and test
  wants a next twin producing identical values and tick times. Parity gaps
  are bugs.

## Ground rules

1. **Legacy is the oracle.** A port must match the classic implementation's
   observable behaviour (values *and* tick times), or document the deviation
   in the capability matrix. Never silently drop a capability.
2. **One mechanism per op.** Semantics live in one `Op::cycle` — no
   duplicated logic per engine, no per-op tables in the macro.
3. **Burst model.** Same-instant values are delivered atomically in one
   `Burst`; nothing is coalesced or dropped.
4. **Fallible, with context.** No `.unwrap()` outside `#[cfg(test)]` and doc
   examples; propagate with `?` and `anyhow::Context` at I/O boundaries.
5. **No locks on the graph path.** Background threads talk to the graph
   through the channel layer.

## Building and testing

From the repository root (the crates are root-workspace members):

```bash
cargo build -p wingfoil-next
cargo test  -p wingfoil-next --all-features
cargo bench -p wingfoil-next          # three-tier regression gate
cargo fmt --all
cargo lint && cargo lint-all          # workspace clippy aliases, mirror CI
```

The default feature set is dependency-free; `--all-features` adds the
`async` (tokio/futures), `csv` and `augurs` adapters.
