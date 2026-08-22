# Wingfoil 9.0: compile your streaming graph with Nitro

Wingfoil 9.0.0 is out today on crates.io, PyPI and npm. The release rebuilds
the engine underneath the library, and the feature that pays for the rebuild is
Nitro: the same graph can now run interpreted, be compiled whole into a single
native function, or mix the two. On the project's benchmarks, compiling takes
engine overhead from about 20 nanoseconds per node per cycle to under half a
nanosecond.

## Compile the parts that need it

You wire a graph once and choose how each part runs:

- interpreted, as before, with the full dynamic surface;
- `compiled()`, the whole graph monomorphized into one function, for when the
  entire pipeline is the hot path;
- `nested()`, a compiled island mounted as one node inside an interpreted
  graph, so the hot path compiles while the surrounding graph keeps doing I/O,
  threaded ingest and dynamic changes.

There is no second system to maintain. An op's behaviour is written once, as a
plain function the engine calls, and the tiers differ only in how the engine
reaches it — the compiled path is not a translation of the interpreted one, so
the two cannot drift apart. Compilation happens at build time, through Rust's
own compiler, not a JIT. One current limit, stated plainly: compiled realtime
has no wake-driven ingest yet, so threaded sources sit at the interpreted
boundary with compiled islands inside them, while busy-poll ingest reaches
both compiled tiers.

## Your own ops are not second-class

Nitro has no registry of blessed operations. A single attribute on your op
derives both the interpreted builder method and the hooks the compiled paths
dispatch through, and it works from your crate exactly as it does in the
built-in catalog. The gap is measured: a 20-stage chain of a user-defined op
compiles to within 2.4% of the same chain built from built-ins, where
"supported but slow" custom logic usually costs an order of magnitude.
Extending the engine and getting compiled speed are the same step.

## Upgrading pays before you touch Nitro

The rebuilt interpreter is itself faster than the engine it replaces,
finishing all eight benchmark workloads in 56% to 84% of the 8.x running
time — a result that was a pass/fail gate on shipping the release. The upgrade
is a real breaking change — custom nodes and the wiring API change shape, and
there is no compatibility layer — but it is a managed one: migration guides
cover Rust and Python, 8.0.0 stays on crates.io, PyPI and npm permanently, and
existing lockfiles keep resolving. For distributed systems, the ZeroMQ wire
format is byte-compatible between 8.x and 9.0 in both directions, covered by
cross-engine tests, so a publisher on the old engine can feed a subscriber on
the new one while you migrate process by process.

## What the benchmarks say

The engine's cost is easiest to state per node, per cycle. The interpreted
tier spends about 20 ns of engine overhead per node per cycle, measured on a
100-node graph with every node ticking on every cycle; an independent depth
sweep reads the same cost as a slope — each node added to a chain adds a fixed
~22 ns. Compiling removes nearly all of that: the whole-program tier's slope
is about 0.35 ns per added node, and a 37-node graph completes an entire cycle
in roughly 19 ns — the whole graph for about the price the interpreter pays on
one node. A compiled island runs its interior at the same cost but pays a
fixed ~55 ns boundary each time the outer graph activates it, which is why
islands earn their keep on hot paths of more than a handful of nodes. Against
per-path libraries the difference is starker: at branch/recombine depth 10,
with 1,024 source-to-sink paths, a compiled Wingfoil cycle costs 23.9 ns and
an interpreted one 287.5 ns, while the same pattern in tokio async streams
costs 38.5 µs per event — a thousand times more — because Wingfoil visits each
node once per tick however many paths lead to it, and per-path propagation
doubles at every level. Quiet graphs pay for activity, not size: an 8-node hot
path inside a 1,030-node graph costs about 294 ns per cycle, tens of
nanoseconds per active node, against 6.05 µs for a dispatcher that sweeps
every node — and that cost tracks active nodes is asserted by a deterministic
test, not a benchmark threshold. One caution on every absolute figure here:
they were captured on low-spec shared cloud VMs — 4-core KVM guests at
2.1–2.8 GHz, not tuned hardware — so treat them as shape rather than spec.
Each comparison was measured back to back in one run, and the cross-library
figures compare a Wingfoil cycle against a per-event call into the other
library. Faster hardware moves the absolute times; the project's one reading
from a dedicated box ran quicker than either VM used here.

Wingfoil 9.0 gives one wiring three ways to run, and the compiled way cuts
engine overhead from about 20 ns per node per cycle to fractions of a
nanosecond. The break with 8.x is real but managed, and rollout can be
gradual.

## About Wingfoil

Wingfoil is an open-source Rust stream processing library: describe a directed
graph of transformations once and run it against live data or replayed history
with identical semantics. It ships adapters for Kafka, ZeroMQ, KDB+, Redis,
Postgres, FIX, Aeron and iceoryx2 shared memory, a statistics library, and
Python and TypeScript bindings. Source and documentation are at
[github.com/wingfoil-io/wingfoil](https://github.com/wingfoil-io/wingfoil),
and more is at [wingfoil.io](https://www.wingfoil.io).
