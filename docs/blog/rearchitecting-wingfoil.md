# Rearchitecting Wingfoil: one definition, three engines

Wingfoil is a Rust library for stream processing: you describe a directed
acyclic graph of transformations once, and run it either against live data or
replayed over history, with identical semantics. It is used where latency
matters — electronic trading, real-time systems — so "how fast is one cycle
through the graph" is not an academic question.

We rewrote the engine. Not refactored: rewrote, in parallel, alongside the
original, with the original kept running as a parity oracle until the new one
was a strict superset of it. This post is about why that was the right call,
the handful of design decisions that everything else follows from, and what
the rewrite actually bought.

## Where we started

The original engine was built around a `MutableNode` trait. A node was a
struct; you wrote its behaviour as a method on `&mut self`; the `#[node]`
attribute macro filled in the plumbing.

```rust
#[node(active = [upstream], output = value: f64)]
impl MutableNode for ScaleStream {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value() * self.factor;
        Ok(true)
    }
}
```

It worked. It shipped. It still ships — a full set of I/O adapters, Python
bindings, a large statistics library, years of production use. The problem was not that it
was wrong; it was that it had fused three separate concerns into a single
object, and that fusion turned out to be load-bearing in a way that blocked
everything we wanted to do next.

A node was:

1. its **computation** (the body of `cycle`),
2. its **storage** (`RefCell` fields on the struct), and
3. its **input plumbing** (peeking at upstream `Rc<dyn Stream>` handles it
   held).

Those three being one thing is invisible until you try to run the computation
somewhere the storage and the plumbing can't follow.

## Why we rewrote: the wall we hit

The trigger was a performance project. An interpreted graph pays, per node per
cycle, a dynamic dispatch and a trip through a value slot behind `Rc<dyn Any>`.
For a hot 20-node chain that overhead dominates the actual arithmetic. The
obvious answer is to generate code: emit a straight-line Rust function that
runs the whole graph with node state in local variables, and let LLVM optimise
across node boundaries.

So we built one — an ahead-of-time code generator that ran the wiring from
`build.rs`, walked the resulting graph, and emitted a runner. It hit three
walls, and all three trace back to the fusion above.

**Wall 1: the generator had to re-implement every node.** Semantics lived
inside `MutableNode` objects, coupled to `RefCell` fields and stored upstream
handles. Generated code could not *call* a node's `cycle` — there was nothing
callable that didn't drag the storage model along. So the emitter carried its
own copy of what each node did, as strings. Two implementations of `map`,
`filter`, `fold`, `delay` — one that runs, one that gets emitted — with nothing
keeping them in step. Every fix to either one is a silent drift risk.

**Wall 2: types and closures were gone by the time we looked.** Wiring erased
them. Types came back from the traversal as *name strings*, so the emitter
reverse-engineered them by parsing. Closures could not be recovered at all —
there is no way to get `|x| x * 10` back out of a `Box<dyn Fn>`. The
workaround was to make the user re-state every closure in a side table for the
generator to splice in. A human re-typing the closure that already exists ten
lines up, with no compiler check that the two agree.

**Wall 3: scheduling was inferred from names.** Nothing on a node declared
"this one schedules its own callbacks" or "this one is fed from another
thread", so the engine relied on name-based allowlists to decide how to drive
it. A user-defined node that needed to self-schedule had no way to say so.

We deleted that generator. Its remains are recorded in the port plan as a
write-off, and the conclusion we drew from it is the whole rewrite: **the
problem was not the code generator, it was that node semantics were not
separately callable.** No amount of cleverness in the emitter fixes a design
where there is nothing to emit a call *to*.

## The one decision everything follows from

Node semantics became an associated function, not a method on an object.

```rust
trait Op {
    type Cfg;              // construction-time config; closures live here
    type State;            // engine-owned mutable state
    type In<'a>;           // typed inputs, passed in per cycle
    type Out;              // the produced value
    const ACTIVATION: Activation;

    fn cycle(cfg: &mut Self::Cfg, state: &mut Self::State,
             input: Self::In<'_>, ctx: &mut Ctx<'_>) -> Result<Tick<Self::Out>>;
}
```

Read what is *absent*. Nothing here says how state is stored, where inputs come
from, or who calls it. `cycle` is a free function over explicit arguments — so
the same function can be driven by completely different machinery:

| Strategy | Who owns state | Dispatch |
|---|---|---|
| **Interpreted** | the builder's slots | one dyn call per node |
| **Compiled** | local variables in a generated fn | monomorphized, inlinable |
| **Nested island** | locals inside one composite node | monomorphized inside, one dyn call outside |

The same source, three execution strategies. Not three implementations kept in
sync by discipline and tests — **one implementation, which is why they cannot
drift.** If you ever find yourself writing a second copy of what a node does,
that is the invariant breaking, and it is the only invariant in the codebase we
treat as non-negotiable.

The three walls fall out at once. Wall 1: there is something callable to emit
calls to. Wall 2: types and closures are ordinary generic parameters, so they
survive into generated code by construction. Wall 3: `const ACTIVATION` states
scheduling behaviour where a machine can read it.

The front door is a macro. You write the wiring once, in plain Rust:

```rust
wingfoil::nitro! {
    fn odds_evens(g: &GraphBuilder) -> Stream<Vec<String>> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        let is_odd = is_even.map(|b| !b);
        let odd_str = count.filter(&is_odd).map(|i| format!("{i} is odd"));
        let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
        odd_str.merge(&even_str).accumulate()
    }
}
```

and get `odds_evens::interpreted()`, `odds_evens::compiled()`, and
`odds_evens::nested()` out of the same tokens. The wiring function is valid,
reviewable Rust either way — a property we deliberately refused to trade away
later.

## The design decisions that shaped the rest

### `Tick` has three states, not two

The old `cycle` returned `Ok(true)` / `Ok(false)`: did it fire? That cannot
express "store a new value but do not tick downstream", which is exactly what
`delay` needs — a passive reader must not see `T::default()` while the delay is
still running. The old engine handled this with special cases in the engine,
next to the node.

```rust
enum Tick<T> { Value(T), Silent(T), Quiet }
```

`Silent` promotes that special case into the contract, so `Delay::cycle`
expresses its full semantics in one place and all three engines handle it
generically. We decided this *early*, as a contract question, before porting a
single node — deciding it late would have meant retrofitting every emitter, and
the whole plan was ordered to avoid exactly that class of rework.

### Scheduling is declared, not inferred

`Activation` is a const on the op: `NONE` (fires when an upstream ticks),
`SCHEDULES` (also wakes itself through the time queue — tickers, delays),
`THREADED` (fed from another thread), `ALWAYS` (busy-spun every cycle — socket
polling). Because it is a const, the interpreted engine reads it at wiring time
and the compiled path folds it into a dispatch condition after
monomorphization. The name-based allowlist is gone, and a user op can be a
first-class self-scheduling node.

### The vocabulary is open; `Stream` is closed

Combinators are **extension traits**, never inherent methods on `Stream<T>`.
Everything goes through two public primitives — `GraphBuilder::source` and
`Stream::wire` — so a downstream crate can add ops that feel native without
this crate knowing they exist. If you are editing a match arm to register an
op, you are on the wrong road.

That principle got its real test when we asked whether a *user's* op could work
inside the compiled path without editing the macro crate. The honest answer
looked like "no": a proc macro expands before name resolution and type
checking, so it cannot know the type of `.my_op()`. The original design had a
per-op table in the macro, and every new op meant a new row.

The way out is to make the compiler do the interrogation. `#[op]` generates
**forwarder functions** by naming convention (`__wf_op_delta_cycle`), each one
generic, with a signature written entirely in associated-type projections of
the op. The macro emits calls to a name; rustc's inference resolves the op's
generics from the argument types — including the node's state local, declared
as a bare `let mut __state = Default::default();` with no type at all, whose
type exists only as the projection `<Delta<T> as Op>::State`. Per-op facts the
macro genuinely needs before monomorphization (activation, passive-edge masks)
are re-emitted as consts that fold away.

The result: the op table was **deleted**. Not shrunk — deleted. The macro knows
two method names of its own, both topology combinators that create N nodes.
Everything else, built-in and user-defined alike, takes one identical path. The
built-in catalog became an ordinary consumer of the extension surface, which is
the best ongoing test of it that exists.

We measured the cost, because "generic fallback" usually means "slower":

| Path | Time | Ratio |
|---|---|---|
| built-in table row, compiled | 241.1 µs | 1.00× |
| **user op through the generic path, compiled** | **243.6 µs** | **1.01×** |
| same user op, interpreted | 2.42 ms | ~10× |

Within noise. After monomorphization LLVM cannot tell them apart.

The first cut was 15% slower, incidentally, because it used a conservative
always-on dirty check where the table row had a static one. That is what the
re-emitted `ACTIVATION` const fixed. Worth stating plainly: the elegant version
was slower until we found the one fact that genuinely had to survive to
compile time.

### Two clocks, and the engine owns the snap

There are two notions of time and conflating them is the mistake to avoid.
**Engine time** is source-driven: in historical mode it is pure logic —
`begin_cycle` pops the earliest scheduled callback and consults no clock at
all — which is what makes replay deterministic. It is the only one business
logic may use. **Wall time** is this cycle's wall-clock snap, for latency
stamping and telemetry, read in *both* modes so that "time spent" means the
same thing in a backtest as it does live.

The kernel takes the wall snap **lazily**, on first read, caching it in a
`Cell` so every later reader in the cycle sees the same instant. A cycle in
which nothing stamps reads the clock zero times. That matters more than it
sounds: a clock read costs ~24 ns on our reference hardware, and the compiled
tier runs a whole small graph in about that. When the tier benchmarks were
re-captured after making the snap lazy, the compiled column moved 14%–70%
faster — on a bar that was running at ~55 ns per cycle, removing a 24 ns read
is most of the work.

The same bug in reverse is instructive. An earlier capture had compiled islands
*slower* than the interpreted engine on all eight workloads, contradicting the
design's own claim. Not hardware: `Ctx::nested` was snapping a fresh
`NanoTime::now()` every time it was built — once per inner node per activation
— putting a TSC read on every node of every island. Islands now borrow the
outer cycle's snap, which is both faster and more correct: an island's ops
agree with the rest of the graph about what "this cycle" means, instead of each
reading its own instant.

### Losslessness over convenience

Same-instant values ride a single `Burst<T>`, delivered atomically. Never
coalesced, never latest-wins, never dropped — identically in realtime and
historical replay. The first cut of the channel layer coalesced same-time
values; the second bumped the monotonic clock to split them apart. Both were
wrong, and both were caught by parity tests against the old engine. A
strictly-monotonic clock means a split same-time group cannot be reassembled
afterwards, so the grouping has to be preserved at the source.

Relatedly: the scheduling queue **deduplicates by design** — pushing a
`(value, time)` pair that is already queued is a no-op, because a node
scheduled twice for the same instant must collapse to one event. It is bounded
on `PartialEq` rather than `Hash + Eq`, specifically so `f64` payloads can flow
through `delay` and `feedback`. Both of those look like bugs to a newcomer.
Both are written up in the architecture doc as rules not to "fix".

### Wiring is pure; I/O happens at `start()`

Every adapter establishes its connections at run start, not at graph
construction. Wiring parses, validates, and rejects the wrong run mode — and
does no I/O. So a graph can be built and tested without a live service, and a
connection failure surfaces during the run with node context instead of during
construction.

The corollary: live sources reject historical mode **at wiring**. A historical
run pulls its input through a bounded, timestamp-gated drain, and an unbounded
live tail has no historical timeline to replay — so it would deadlock at
`start`. Rejecting at wiring turns a hang into an error message that names the
bounded reader you wanted instead.

### Fallible everywhere

Every lifecycle function returns `anyhow::Result`. The interpreted runner
reports the first `start`/`cycle`/`stop`/`teardown` error with node context
(`node 2 (try_map) cycle: boom …`) and runs cleanup regardless; the compiled
path threads the same `?` through and captures the first error so every node's
`stop` and `teardown` still run after an abort. `Result<Tick<T>>` rather than a
four-variant enum, deliberately: `Quiet` is control flow on the hot path and
`Err` is failure on a cold one, and keeping them separate preserves `?`,
`.context()`, and the anyhow chain inside op bodies. For infallible ops the
compiled path constructs `Ok(Tick::Value(x))` and matches it immediately; LLVM
folds the discriminant away and no branch survives in the binary.

### The strategic one: port in parallel, never in place

The new engine was built beside the old one, in the same repository, with the
old tree's test suite as a permanent parity oracle. Every ported node had to
produce identical values **and identical tick times** — a test that only checks
values will not catch a scheduling regression. Every deviation went into a
register and needed an explicit accept-or-fix ruling before cutover; none was
allowed to stay implicit.

Two properties made this affordable. First, the port could pause indefinitely
at any phase boundary with everything already shipped still correct. Second,
the shared runtime core — engine time, run bounds, the time queue, bursts, the
kernel, the latency data layer — physically moved to the *new* crate, and the
old one re-exports it at its historical paths. Both engines use one set of
types, not two structurally-identical twins, so a value crosses the boundary
without conversion. And because the dependency edge points old → new, the
cutover is a deletion rather than a re-organisation.

That inversion is worth dwelling on. The natural instinct is to have the new
thing depend on the old one during a migration. It makes the first month
easier and every subsequent month worse, because at the end you have to unpick
the edge before you can delete anything. Pointing it the other way front-loads
the pain into one refactor and makes the final step `rm -rf`.

## The outcomes

### The interpreted engine got faster too

The gate we set was that the new interpreted engine must be **at least as
fast** as the old one — because most graphs will keep running interpreted, and
a rewrite that trades everyday performance for a fast path nobody uses is a bad
trade. Eight workloads, run back to back on the same machine:

| Workload | Nodes | legacy | interpreted | compiled | nested | interp/legacy |
|---|---|---|---|---|---|---|
| `dense_chain` | 37 | 7.95 ms | 6.64 ms | 187 µs | 931 µs | **0.84×** |
| `fanout` | 103 | 17.05 ms | 11.99 ms | 324 µs | 1.43 ms | **0.70×** |
| `fan_in_16` | 20 | 4.76 ms | 2.67 ms | 174 µs | 855 µs | **0.56×** |
| `fan_in_64` | 68 | 10.72 ms | 6.47 ms | 259 µs | 938 µs | **0.60×** |
| `fan_in_256` | 260 | 38.08 ms | 31.60 ms | 2.55 ms | 3.10 ms | **0.83×** |
| `accumulate` | 3 | 2.08 ms | 1.43 ms | 326 µs | 1.40 ms | **0.68×** |
| `sparse` | 205 | 2.50 ms | 2.11 ms | 311 µs | 751 µs | **0.84×** |
| `sparse_wide` | 781 | 3.07 ms | 2.01 ms | 355 µs | 896 µs | **0.66×** |

Interpreted lands at 0.56×–0.84× of the old engine — faster on every workload.
Compiled beats interpreted by 4.4× (on a three-node graph, where the scheduler
loop rather than dispatch dominates) up to 37× (dense fan-out, its home
ground). Islands land in between, 2.2×–10.2×, except on the three-node graph
where they are a wash — an island needs something to amortise its boundary
against.

These are wall-clock numbers from a shared 4-core cloud VM, so read the ratios,
not the absolute times; every comparison above is between bars measured back to
back in the same run. The benchmarks are deliberately *not* a CI gate —
criterion thresholds are too noisy on shared runners. The perf claims we
actually gate are written as deterministic tests instead: that per-cycle work
is a function of *active* nodes and not graph size is asserted by a test, not
inferred from a chart.

### Sparse graphs

Most real graphs are mostly quiet most of the time. The dirty-list scheduler
does per-cycle work proportional to active nodes, and we kept the naive
full-sweep dispatcher as an executable oracle — same results, both strategies,
which is what makes it useful. On an ~8-node hot path inside a ~1030-node
graph:

| Dispatch | Per cycle |
|---|---|
| sparse (default) | 984 ns |
| full sweep | 6.05 µs |

6.2× apart.

### Against other approaches

The branch-and-recombine pattern, at depths 1–10, where each level doubles the
number of distinct source→sink paths. Wingfoil visits every node once per tick
and stays flat; libraries that propagate one path at a time double per level
(2.01× and 1.94× measured). At depth 10 the interpreted engine is ~39× faster
than rxrust and ~66× faster than tokio async streams; at depth 20 the same
slopes put the gap in the millions. The flatness is the claim, not the
multiplier — and the multipliers are lower bounds, because only wingfoil pays
the benchmark harness's thread handshake.

With that handshake divided out, the actual scaling law is **≈ 68 ns + 22 ns ×
depth** interpreted — one more node per level, while the path count runs to
1024. The whole-program compiled tier costs **~22 ns for the entire graph at
every depth** (21.1 ns at depth 1, 23.9 ns at depth 10): about what the
interpreter pays for a single node.

### The extensibility outcome, restated

A user-defined op gets interpreted *and* fully-compiled coverage, at 1.01× of a
built-in, with no edit to the macro crate and no table to register in. That was
the property we most doubted was achievable, and it is the one that most
changes what the library is for.

### Parity

Everything the old tree did, the new engine does: the whole node catalog, all
the adapters (CSV, Kafka, ZeroMQ, KDB+, Redis, Postgres, etcd, FIX, web, Aeron,
iceoryx2, Fluvio, augurs, Prometheus, OTLP, line files), the statistics
library, dynamic graph mutation, thread offload, async producers and consumers,
the Python bindings, the examples, the benchmarks. One public API was dropped
deliberately: a GML topology dump, which we would rather replace with a
designed introspection story than port in the same shape. The ZeroMQ wire
format stays byte-compatible between the two engines in both directions,
specifically so a staged rollout is safe.

## What it cost

An honest ledger, because a post that only lists wins is not useful to anyone
deciding whether to do this.

**There is no compatibility facade.** We planned one, built the design for it,
and then ruled against it: the old wiring path retires with the old tree and
Rust downstreams break at the major version bump. A facade would have to be
*maintained* across exactly the refactors the cutover exists to enable, and the
Python binding had already made the same call. The migration guide is the
answer instead.

**The compiled tier is deliberately narrower than the interpreted one.**
Feedback loops, busy-poll ingest, threaded sources, and observing arbitrary
intermediate streams are interpreted-only. These are not gaps — the tier's
value comes from the constraint, I/O belongs at the interpreted boundary with
compiled islands inside it, and the old engine had no compiled tier at all so
none of them is a regression against anything.

**Some idioms changed and will annoy people.** Per-node mutable state used to
live in struct fields; now it lives in fold accumulators, because a *mutating
capture* in a combinator closure would behave differently between the
interpreted and compiled engines — so it is a compile error. Both express
arbitrary per-node state; one of them is a habit you have to unlearn.

**We spent effort on things we deleted.** The code generator, most obviously.
And an n-ary `merge` shipped first as sugar — a chain of 2-ary merges — on the
sound reasoning that the tie-break is associative so results are identical. The
reasoning was right and the conclusion wrong: identical *results* is not
identical *cost*, and the chain's extra nodes and extra depth measured up to
1.86× the old engine on a wide fan-in, violating the performance gate. It was
replaced with a real variadic op. "Identical results" is the only one of those
two claims a parity suite can check for you.

**Known-deferred, with the trigger written down.** An arena/SoA value store —
the measurement says slot aliasing could recover 4.3× on large-payload
forwarding, which is a big number and exactly why we want a real workload
demanding it before touching the slot representation. Multi-output compiled
islands, deferred alongside it because they share that coupling. Both are
recorded with what would trigger them, which is the difference between deferred
and forgotten.

## Three things we would tell someone doing this

**Decide the contract questions first, in blast-radius order.** Fallibility
came before everything because it touched every op; `Tick::Silent` was settled
as a contract question rather than discovered while porting `delay`. Deciding
either one late means retrofitting every emitter.

**Point the dependency edge at the new thing.** The old tree depends on the new
one, not the reverse, so the shared core was already on the right side when the
cutover arrived. It makes the last step a deletion rather than an
archaeological dig.

**Make the invariant structural, not aspirational.** "Keep the interpreted and
compiled paths in sync" is a promise that decays. "There is exactly one copy of
the semantics, and the second copy is a compile error" is a property. The
entire rewrite is one long argument for preferring the second kind — and the
test that best encodes it is the one that runs the same wiring through all
three engines and asserts they agree, so the property stays true rather than
merely intended.

---

*Wingfoil is on [GitHub](https://github.com/wingfoil-io/wingfoil). The
architecture doc is [`docs/wingfoil-next-architecture.md`](../wingfoil-next-architecture.md);
the migration guide is [`docs/migration.md`](../migration.md); the benchmark
methodology, machines and full numbers are in
[`crates/wingfoil/benches/README.md`](../../crates/wingfoil/benches/README.md).*
