# Wingfoil 9.0 replaces its engine and ships Nitro, compiled execution for dataflow graphs

Wingfoil 9.0.0 is available today on crates.io, PyPI and npm. It replaces the
engine underneath the library: the `MutableNode` core that shipped
through the 8.x line is superseded by the `Op` engine, a deliberate breaking
change with no compatibility facade. The headline feature is Nitro, a tier
system that can compile an entire dataflow graph into a single function. On the
project's eight benchmark workloads, the compiled tier runs 4.4× to 37× faster
than the interpreted one.

## One copy of the semantics, three ways to execute it

Nitro exists because of a change in where node semantics live. In 8.x a node
was one object that fused its computation, its storage and its input plumbing,
so the engine could only walk a list of trait objects and call them; there was
nothing to monomorphize. In 9.0 an op declares only what it computes, as an
associated function over explicit arguments: `cycle(cfg, state, input, ctx)`.
The engine owns the state and hands in the inputs, so the same function can be
driven three ways:

- interpreted, one dynamic call per node, with state in the builder's slots;
- `compiled()`, the whole graph monomorphized into one generated function with
  state in locals;
- `nested()`, a compiled island mounted as a single node inside an interpreted
  graph.

The semantics exist once, in each op's `cycle`, and the tiers differ only in
how the engine reaches it, so they cannot drift apart. One limit is stated up
front: compiled realtime has no wake-driven ingest yet, so threaded sources sit
at the interpreted boundary with compiled islands inside; busy-poll ingest
reaches both compiled tiers.

## User ops take the identical path

Nitro has no per-op registry. The `#[op(build = name)]` attribute derives both
the interpreted builder method and the forwarders the compiled paths dispatch
through, from the declared shape of the op, and it works from a downstream
crate exactly as it does in the built-in catalog. The claim is measured: a
20-stage chain of a user op driven through the generic compiled fallback
completes in 622 µs where the same chain built from built-ins takes 608 µs, a
2.4% difference, and both are about 9× faster than the interpreted run.

## A deliberate break, with a way across

There is no compatibility facade, and that was ruled explicitly: a facade would
have to be maintained across exactly the refactors the new engine enables, and
it would keep the fused node shape alive as a supported surface. Instead,
8.0.0 remains on crates.io, PyPI and npm permanently, existing lockfiles keep
resolving, and migration guides cover Rust and Python. For staged
rollouts, the ZeroMQ wire format is byte-compatible between 8.x and 9.0 in
both directions, including through the Python bindings, and is covered by
cross-engine tests, so a publisher on the old engine can feed a subscriber on
the new one while a system migrates process by process.

## Benchmarks

Across eight fixed-cycle workloads of 3 to 781 nodes, the compiled tier is
4.4× to 37× faster than interpreted and a nested
island is 2.2× to 10.2× faster, except on a three-node workload where the
island's boundary cost makes it a wash. The interpreted tier also beats the
engine it replaces on all eight workloads, at 0.56× to 0.84× of the 8.x time;
that result was a pass/fail gate on the cutover. In absolute terms, the
37-node `dense_chain` graph completes 10,000 compiled cycles in 187 µs, about
19 ns per cycle for the whole graph. On a branch/recombine graph at depth 10, where
1,024 distinct paths run from source to sink, compiled Wingfoil is 1,610×
faster than tokio async streams and the interpreted tier is 134× faster,
because the graph is topologically sorted and each node is visited once per
tick however many paths lead to it, while per-path propagation doubles at
every level. Read the ratios, not the absolute times: these figures were
captured on shared 4-core cloud VMs, every comparison measured back to back in
one run, and the cross-library figures compare a Wingfoil cycle against a
per-event call into the other library.

Wingfoil 9.0 ships one definition of every op and three ways to run it, the
fastest 4.4× to 37× ahead of the interpreted tier, which itself outruns the
engine it replaces. The break with 8.x is documented and optional in timing,
since 8.0.0 stays installable and the wire format bridges the two engines.

## About Wingfoil

Wingfoil is an open-source Rust stream processing library for building
directed acyclic graphs of data transformations, with Python and TypeScript
bindings. The same graph replays deterministically over historical data and
runs live, so a backtest and the production system share one wiring. Source,
benchmarks and documentation are at
[github.com/wingfoil-io/wingfoil](https://github.com/wingfoil-io/wingfoil),
and more is at [wingfoil.io](https://www.wingfoil.io).
