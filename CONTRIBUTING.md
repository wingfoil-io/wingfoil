# Contributing to Wingfoil

We'd love your help. Say hi on [Discord](https://discord.gg/rfGqf3Ff), open a
[discussion](https://github.com/wingfoil-io/wingfoil/discussions), or comment
on any issue you fancy.

## Getting set up

You need the Rust toolchain (latest stable, with `rustfmt` and `clippy`) and
`protoc` — a transitive dependency builds proto files, so a plain workspace
build needs it:

```bash
scripts/setup-dev.sh              # installs protoc; Debian/Ubuntu and macOS
```

Then check everything works end to end:

```bash
git clone https://github.com/wingfoil-io/wingfoil.git && cd wingfoil
cargo test --manifest-path crates/wingfoil/Cargo.toml
cargo run  --manifest-path crates/wingfoil/Cargo.toml --example hello_graph
```

A few adapters need more (Aeron wants clang, libuuid and CMake ≥ 3.20; some
adapter tests want a server) — [`CLAUDE.md`](CLAUDE.md) has the details, and
none of it is needed to work on the engine.

**Where to start:** the
[`good first issue`](https://github.com/wingfoil-io/wingfoil/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
label, or anything labelled `size: small`. Issues also carry `priority:` and
area labels (`core`, `io-adapter`, `python`) if you want to browse by
interest. Not sure whether an idea fits? Ask first in an issue or on Discord —
that is cheaper for both of us than a PR that has to be unwound.

**Read first:** [`docs/wingfoil-architecture.md`](docs/wingfoil-architecture.md)
is the shape of the engine and the one decision everything else follows from.
Worth 20 minutes before your first non-trivial change.

## How the work is organised

Wingfoil is a ground-up rebuild of the legacy engine
([`legacy/CONTRIBUTING.md`](legacy/CONTRIBUTING.md)) on the Op pattern — see
[`README.md`](README.md) for the design objectives and
[`docs/port-plan.md`](docs/port-plan.md) for the roadmap.

Two trees, two workflows, and it matters which one you are in:

| You are changing | Branch from | PR targets |
|---|---|---|
| Anything outside `legacy/` | `next` | `next` |
| Anything under `legacy/` | `main` | `main` |

Never commit directly to `next` or `main`. Branch names are simple and
descriptive — `add-metrics`, `fix-error-handling`.

## What contributions look like here

The port advances phase by phase (see the plan's ✅/🟡/⬜ markers). The most
valuable contributions are:

- **Porting a legacy node/operator** — follow "Adding an op" in
  [`docs/port-plan.md`](docs/port-plan.md). Most single-input ops need only
  an `Op` impl with `#[op(build = ...)]` plus a 3-line fluent method; the
  compiled path is zero-touch.
- **Porting a legacy adapter** — follow the `/new-adapter` skill
  (`.claude/commands/new-adapter.md` from the repo root), which encodes
  the layering rules (sources over `channel`/`poll`, sinks over `for_each`,
  extension traits out of the prelude).
- **Porting a legacy example or test** — every legacy example and test
  wants a wingfoil twin producing identical values and tick times. Parity gaps
  are bugs.

## Ground rules

1. **Legacy is the oracle.** A port must match the legacy implementation's
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
cargo build --manifest-path crates/wingfoil/Cargo.toml
cargo test  --manifest-path crates/wingfoil/Cargo.toml --all-features
cargo bench --manifest-path crates/wingfoil/Cargo.toml          # three-tier regression gate
cargo fmt --all
cargo lint && cargo lint-all          # workspace clippy aliases, mirror CI
```

`legacy/` is **not** in this workspace — it is its own, so none of the above
reaches it and `--manifest-path crates/wingfoil/Cargo.toml` does not resolve here. See
[`legacy/CONTRIBUTING.md`](legacy/CONTRIBUTING.md#pre-pr-check-matches-ci) if
you are changing that tree.

The default feature set is dependency-free; `--all-features` adds the
`async` (tokio/futures), `csv` and `augurs` adapters.
