## Two-pass codegen — a compiled graph from a config file

`nitro!` reads tokens, so the shape of its graph is fixed when you compile.
That rules out the thing a real desk needs most: **N pipelines, where N comes
from a config file**. [`dual_mode`](../dual_mode/) states the rule plainly —
*wiring must be straight-line; the topology cannot depend on runtime values*.

This example goes around it. Pass 1 **runs** the loop against the interpreted
builder, walks the graph it produced, and prints the unrolled pipelines as
`nitro!` input. Pass 2 is an ordinary `cargo build` over that text. The macro
never sees a loop — it sees the two pipelines the loop already built.

```text
  config ──▶ pass 1: run the wiring ──▶ walk the graph ──▶ desk.gen.rs
                (interpreted)                              (nitro! input)
                                                                │
                                       pass 2: cargo build ◀────┘
                                                │
                                          compiled runner
```

Both passes are visible here. `desk` in [`main.rs`](main.rs) is pass 1;
[`desk.gen.rs`](desk.gen.rs) is what it produced, checked in beside it and
`include!`d — so pass 2 happened when you built this binary, and the two run
side by side at the end.

### The wiring is ordinary Rust

```rust,ignore
#[wiring]
fn desk(g: &GraphBuilder, book: &[Instrument]) -> Stream<f64> {
    book.iter()
        .map(|inst| {
            let fee = inst.fee;
            let size = inst.size;

            g.ticker(inst.period)
                .count()
                .map(move |n: &u64| (n * size) as f64)
                .map(move |notional: &f64| notional - fee)
        })
        .reduce(|a, b| a.join(&b, |x: &f64, y: &f64| x + y))
        .expect("the book has at least one instrument")
}
```

That is the entire annotation burden: one attribute. No `func!`, no `_q` twin,
no `with_src`, no `with_cfg`, and **no capture list** — `#[wiring]` records every
closure it sees and finds `fee` and `size` itself by free-variable analysis.

The engine erases closures (a node's cycle is a `Box<dyn FnMut>`), so without
that the bodies would be gone by the time a traversal looked. The attribute
rewrites each closure-carrying call to keep the tokens, and renders each
detected capture through `EmitLiteral` so the artifact can re-materialise it.

### Two things worth watching in the output

**The parameters are baked in, per instrument.** `let size = 50u64;` in one
pipeline and `let size = 20u64;` in the other. That is partial evaluation, and
it is the point — but it also means the artifact is **frozen**: change a fee and
you must regenerate. `check_artifact` is what turns forgetting into a failing
test rather than a wrong number in production.

**Anything unemittable is refused, never partially emitted.** The example ends
by trying to generate from a closure capturing an `Arc<Mutex<_>>`. That graph
*wires and runs* perfectly well — only generating from it is refused, and the
message names the binding. Capture detection renders softly on purpose: an
`EmitLiteral` bound on every detected capture would make ordinary wiring fail to
compile.

### Output

```text
Config, read at run time — the compiler never saw any of it:
  ESZ5   period   1ms   fee  0.25   size  50
  NQZ5   period   4ms   fee  1.75   size  20

Pass 1 — running the wiring emits this `nitro!` input:

  wingfoil::nitro! {
      fn desk_generated(g: &GraphBuilder) -> Stream<f64> {
          let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
          let n1 = n0.count();
          let n2 = n1.map({ let size = 50u64; move |n: &u64| (n * size) as f64 });
          let n3 = n2.map({ let fee = 0.25f64; move |notional: &f64| notional - fee });
          let n4 = g.ticker(::core::time::Duration::new(0u64, 4000000u32));
          let n5 = n4.count();
          let n6 = n5.map({ let size = 20u64; move |n: &u64| (n * size) as f64 });
          let n7 = n6.map({ let fee = 1.75f64; move |notional: &f64| notional - fee });
          let n8 = n3.join(&n7, |x: &f64, y: &f64| x + y);
          n8
      }
  }

  2 instruments -> 2 tickers, 0 loops in the artifact.
  The checked-in artifact is current.

Pass 2 — the artifact, compiled into this binary:

  interpreted wiring : [68.0, 118.0, 168.0, 218.0, 288.0, 338.0, 388.0, 438.0]
  compiled artifact  : 438.0 (final value)

  They agree — the artifact computes what the wiring computed.

What a refusal looks like:

  1 node(s) of this graph cannot be emitted:
    - node 2 (Map): the closure captures `journal`, whose value cannot be rendered as Rust source — no `EmitLiteral` impl. ...
```

### Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example codegen
```

Change the config or the wiring and the checked-in artifact goes stale, which
the plain run reports as an error. Regenerate it with:

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example codegen -- --regenerate
```

### What it cannot do

The artifact is `nitro!` input, so it inherits every `nitro!` limit: `merge` and
`join` are two-input on the compiled path, the topology is static once
generated, and `compiled()` takes no wake-driven I/O. Variadic ops
(`merge_all`, `combine`) carry no `#[op(build = …)]` name and are refused.

### Where to go next

- [`dual_mode`](../dual_mode/) — the rule this example works around, and what
  `nitro!` expands to.
- [`hello_graph`](../hello_graph/) — the wiring vocabulary, if this is your
  first stop.
