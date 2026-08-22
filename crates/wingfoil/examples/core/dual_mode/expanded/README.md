# The generated code, verbatim

[`main.expanded.rs`](main.expanded.rs) is the **unedited** output of expanding
[`../main.rs`](../main.rs) — the whole example file with the `nitro!` block
replaced by the module it generates: `wire` (the wiring function, verbatim),
`interpreted`, `compiled`, `run`, and `nested`. It is committed so "straight-line
wiring becomes a static schedule" is something you can read in real emitted
code, not an abridged rendering.

It is a **reference snapshot, not a build target** — nothing compiles it (the
example's only declared target is `../main.rs`), and the pretty-printer's output
is not valid stable Rust anyway (it opens with `#![feature(prelude_import)]`
and names `::alloc` paths). Two artifacts of `-Zunpretty=expanded` to read
around: `format!` and `log::info!` appear pre-expanded into
`::alloc::fmt::format(format_args!(..))` / `::log::__private_api::log(..)`
forms, and comments from the source file are dropped or relocated.

## What to look at

- **`compiled`** — no graph object at all: one `(cfg?, state, value)` triple of
  stack locals per node, seeded through `__wf_op_<op>_seed_*` forwarders; the
  cycle loop is the whole schedule, straight-line, with tick propagation as
  plain `bool`s and every closure inlined at its call site. The
  `__WF_OP_<OP>_{ACTIVATION,PASSIVE}` consts fold into the tick gates after
  monomorphization.
- **`nested`** — the same 10-node schedule mounted as a *single* compiled node
  inside an interpreted graph (`__g.__composite(..)`), with a private
  `TimeQueue` demultiplexing the ticker's inner schedules.
- The macro never names an op type: rustc's inference resolves each forwarder
  from the argument types at the call site, which is why `#[op]` gives a
  user-defined op the identical path with no macro table to edit.

## Regenerating

The snapshot is pinned to the `main.rs` beside it — regenerate it whenever that
file changes (a nightly toolchain is required, as macro expansion is a nightly
`rustc` flag):

```sh
cargo +nightly rustc -p wingfoil --example dual_mode --profile check \
    -- -Zunpretty=expanded > crates/wingfoil/examples/core/dual_mode/expanded/main.expanded.rs
```

(`cargo expand -p wingfoil --example dual_mode` prints the same thing with
syntax highlighting, if you have `cargo-expand` installed.)
