# `dyn` vs generics in Rust: where it matters, where it doesn't

There is a lot of folklore that "dynamic dispatch is slow", and it gets used as
an argument against designs like Wingfoil's — which wire graphs out of
`Rc<dyn Node>` and boxed closures. This benchmark ([`dyn_vs_generic.rs`](dyn_vs_generic.rs))
measures the trade-off directly so it can be reasoned about with numbers.

**TL;DR:** the cost of `dyn` is not the vtable. A well-predicted virtual call is
~1 ns. The cost is that a virtual call is an **optimisation barrier** — the
compiler can't inline through it, so it loses constant-folding, auto-vectorisation
and loop fusion. That makes the overhead *enormous* when each call does almost
nothing, and *immaterial* the moment each call does real work.

## Results

One machine (2.1 GHz Xeon, `rustc` 1.94, `--release`). Absolute nanoseconds
won't travel to your laptop; the **ratios** will. Each row runs the *same source
operation* under static (monomorphised/generic) vs dynamic (`dyn`) dispatch —
only the dispatch mechanism changes.

| Benchmark | What each call does | Static | Dynamic (`dyn`) | Slowdown |
|---|---|---:|---:|---:|
| `tiny_op` | `x*2+1` (one mul, one add) | 0.21 ns/elem | 1.78 ns/elem | **8.5×** |
| `heavy_op` | 24-round integer mix (~19 ns) | 19.0 ns/elem | 19.3 ns/elem | **1.02×** |
| `pipeline_6_stages` | 6 trivial ops chained | 0.68 ns/elem | 12.4 ns/elem | **18×** |
| `stateful_nodes` | a node's `cycle()`: mix input into state | 0.75 ns/node | 1.84 ns/node | **2.5×** |

Plus a second-order effect — even within `dyn`, the call target's predictability
matters:

| Benchmark | Call site | Cost per call |
|---|---|---:|
| `dyn_dispatch_prediction` | same target every call (monomorphic) | 1.44 ns |
| `dyn_dispatch_prediction` | target rotates every call (megamorphic) | 1.88 ns |

## What the numbers actually say

**1. The overhead is a roughly fixed ~1–2 ns per call. Whether it matters is
entirely the denominator.**

- `tiny_op`: the operation is *cheaper than a function call*, and inlining the
  static version lets the compiler vectorise the whole loop. `dyn` forces one
  opaque call per element — so it's ~8× slower. This is the scary "dyn is slow"
  headline. It is real, and it only exists here.
- `heavy_op`: identical harness, but now each call does ~19 ns of genuine work.
  The ~1 ns of dispatch is lost in the noise — **1.5%**. This is the regime
  almost all application code lives in.

**2. The barrier compounds with pipeline depth — this is the biggest effect.**
In `pipeline_6_stages` the static version fuses six stages into a single tight
(vectorisable) loop body: 0.68 ns for all six. The `dyn` version makes six
separate boxed-closure calls per element, each an optimisation barrier: ~2 ns ×
6 = ~12 ns. That's **18×**. If you're going to chain many trivial transforms in
a hot loop, keep them generic so they fuse.

**3. Not all `dyn` calls cost the same.** A call site that always dispatches to
the *same* concrete type is branch-predicted almost for free (~1.44 ns). One
whose target keeps changing (a "megamorphic" site) eats branch mispredicts
(~1.88 ns). So `Vec<Box<dyn Trait>>` where every element is the same type is
cheaper than a genuinely mixed one — the folklore rarely mentions this.

**4. The case that actually maps to a graph engine — `stateful_nodes` — is
2.5×, and immaterial in context.** Each node's `cycle()` does a small but real
piece of work. Dispatch adds ~1.2 ns per node. Wingfoil's measured graph
overhead is ~20 ns per node cycle, so dispatch is ~6% of the framework overhead —
and the node's *actual* user work (peeking upstream, running the user closure,
storing, sometimes scheduling) sits on top of that. In a real graph the number
disappears.

## So why does Wingfoil choose `dyn`? (The nested-types question)

Because a DAG is **heterogeneous by nature**. A graph's node list holds a
`MapStream`, a `FilterStream`, a `FoldStream`, a `Delay`, … — different concrete
types that must live in the *same* container and be wired together at runtime.
You cannot put those in a `Vec<T>`. The moment you need a homogeneous collection
of differently-typed things, you need type erasure — `dyn` — regardless of the
performance argument.

The generic alternative doesn't just get slower to compile; the *types stop
being usable*. Here is the type of a 6-stage iterator pipeline, fully
monomorphised (from [`typenames.rs`](dyn_vs_generic/typenames.rs)):

```
Map<Filter<Map<Map<Filter<Map<Range<u64>, {closure}>, {closure}>,
    {closure}>, {closure}>, {closure}>, {closure}>
```

Every combinator wraps the previous type, so the type grows with the pipeline.
Type-erase after each step (what a `dyn`-based fluent API does) and it collapses,
at *any* depth, to:

```
Box<dyn Iterator<Item = u64>>
```

The "extreme nested types" worry is genuine, and it bites in four concrete ways
before you ever measure speed:

1. **Return types become unnameable** — you can't write the type down; you need
   `impl Trait` (which can't cross a `Vec` or a struct field with many variants)
   or a box.
2. **Error messages** turn into walls of `Map<Filter<Map<…>>>`.
3. **Compile time and binary size** blow up: monomorphisation emits a fresh copy
   of every function for every unique type combination.
4. **You still have to box** the instant you want to store nodes in a `Vec`, hold
   one in a struct with a fixed field type, or decide the shape at runtime — which
   is exactly what a graph does.

`Iterator` gets away with being fully generic because a pipeline is *fixed and
known at compile time* and lives in one hot loop — the perfect case for static
dispatch (see `tiny_op`/`pipeline_6_stages`, where it wins big). A stream graph
is the opposite: its shape is data, decided at wiring time. `dyn` isn't a
performance concession there — it's the only thing that expresses the problem.

## Practical rule of thumb

- Put `dyn` at **coarse boundaries** — per-node, per-message, per-request — where
  each call already does real work. The overhead is noise (`heavy_op`,
  `stateful_nodes`).
- Keep the **innermost numeric kernel** generic/monomorphised, so it inlines,
  fuses and vectorises (`tiny_op`, `pipeline_6_stages`).
- Don't reach for generics to shave 1 ns off a call that does 20 ns of work —
  you'll pay for it in compile time, binary size and unreadable types, and the
  benchmark won't move.

Reproduce with:

```bash
cargo bench --bench dyn_vs_generic
```
