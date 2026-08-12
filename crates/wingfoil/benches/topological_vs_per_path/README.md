## Topological Sort vs Per-Path Propagation: Branch / Recombine Benchmark

Port of `legacy/wingfoil/benches/bfs_vs_dfs/README.md` onto the wingfoil engine.

> The headline charts and the summary this data supports are in
> [`../README.md`](../README.md#flat-where-reactive-doubles). **This page is the
> full per-depth numbers, the method, and every caveat** — read it before
> quoting a multiplier from the parent.

These benchmarks measure the cost of the branch/recombine pattern at depths 1–10:

<img src="diagram.png" width="200" align="centre">

At depth N the graph has 2^N paths from source to sink. The execution model
determines whether a framework pays O(N) or O(2^N) per tick.

### How the wingfoil side is measured

Each depth is one `nitro!` block wiring a **self-contained** graph — the driving
ticker lives inside the block — run for a fixed 10 000 cycles per sample. The
reported figure is whole-run time divided by the cycle count, so nothing but the
graph is under the measurement.

That is a change from how this bench used to work, and worth stating plainly
because older captures of it are still around. The target previously carried a
second sweep driven through `wingfoil::bencher::add_bench`, which runs the graph
on a worker thread and hands off one tick per criterion sample through a
spinning `AtomicU8`. The handshake cost a few hundred nanoseconds against graphs
of 4–13 nodes costing tens, so those samples were mostly harness: flat long
before the graph was, non-monotonic in depth, and useless for reading a
per-depth figure. It has been removed rather than estimated and subtracted.
Two consequences:

- **All three engine tiers are measurable now.** `nitro!` emits a whole-program
  `compiled()` only for a graph with no stream parameters; an input-taking graph
  is a component by definition and gets `wire` + `nested` only. A graph fed by
  the bencher must take an input, so `compiled()` could never appear under the
  old harness.
- **The comparison against legacy is no longer like-for-like.** Legacy's
  `bfs_vs_dfs` still measures one tick per sample through its own `bencher`, so
  its `depth_N` numbers and these are not the same measurement. The workload is
  unchanged node-for-node; the timing method is not.

### Results

<img src="per_cycle.png" width="640">

Nanoseconds per cycle, by tier:

| Depth | Nodes | interpreted | compiled | compiled island |
|---|---|---|---|---|
| 1  | 4  | 87.0  | **21.1** | 73.8 |
| 2  | 5  | 116.4 | **22.2** | 78.1 |
| 3  | 6  | 135.5 | **21.9** | 80.1 |
| 4  | 7  | 150.3 | **23.7** | 73.4 |
| 5  | 8  | 179.7 | **23.3** | 78.6 |
| 6  | 9  | 199.0 | **24.5** | 74.6 |
| 7  | 10 | 217.5 | **24.7** | 81.8 |
| 8  | 11 | 259.0 | **23.5** | 72.7 |
| 9  | 12 | 257.4 | **25.2** | 80.5 |
| 10 | 13 | 287.5 | **23.9** | 86.0 |
| *least squares* | | 68 + **22.0**·d | 21 + **0.35**·d | 74 + **0.67**·d |

Three things to read off it:

- **The O(N) claim, as a number.** Least squares over the ten depths puts the
  interpreted engine at **≈ 68 ns + 22.0 ns × depth**: one more level is one more
  node and a fixed ~22 ns, every tick. Across the sweep the path count grows
  512-fold (2 → 1024) while the cost grows 3.3×. Node-cycle throughput is the
  same claim read sideways — it sits at ~46 M/s regardless of depth, i.e. cost
  tracks node count, not path count.

  The two previous captures read ≈ 97 + 22 ns × depth and ≈ 55 + 21.8 ns × depth.
  **The slope is what the claim rests on, and across three captures it has not
  moved.** The intercept is the part that wanders with the machine and with the
  wall-clock work (`Kernel::wall_time` became lazy between the first two).
- **The whole-program tier is ~22 ns for the entire graph, at any depth** —
  21.1 ns at depth 1, 23.9 ns at depth 10, a marginal 0.35 ns per level. That is
  roughly what the *interpreter* pays for one node. At depth 10 it is 12.0× the
  interpreter and 3.6× the island, and its throughput climbs 190 → 545 M
  node-cycles/s across the sweep purely because nodes get added and wall time
  does not move.
- **The island is flat too, but pays a boundary** — 73.8 ns at depth 1, 86.0 ns
  at depth 10, marginal 0.67 ns per level. Its interior is the *same*
  monomorphized code `compiled()` emits, so the ~55 ns separating the two lines
  is entirely the island's boundary: one outer dyn call, the private
  `TimeQueue`, the mini `begin_cycle` per activation. That is a fixed cost and
  these graphs are 4–13 nodes, so there is nothing to amortise it against —
  the same reason [`../tiers.rs`](../tiers.rs) reports its thinnest island
  margin (1.0×) on `accumulate`, its 3-node workload. The island is not weak
  here; the workload is too small to pay for a boundary.

  Against the interpreter it leads 1.18× at depth 1 and 3.3× at depth 10, and
  the gap widens because only one of the two lines has a slope.

Note the added work is genuinely *executed*, not optimized away: every level's
sum passes through `black_box`, which [`wingfoil.rs`](wingfoil.rs)'s module doc
puts there specifically so the compiled tiers cannot notice that summing one
value 2^N times is a shift and collapse the whole chain. What compilation
removes is the *dispatch* around the add — the interpreter's ~22 ns of dyn call,
`RefCell` borrow and slot clone — not the add.

The island line used to read ≈ 153 ns + 31 ns × depth — *steeper* than the
interpreter's — because every inner node snapped its own `NanoTime::now()`
(~24 ns, see the [`nanotime`](../README.md#the-clock) bench) when its
`Ctx::nested` was built. The island now shares the outer cycle's wall snap. If
you are comparing against a capture from before that fix, that is the whole
difference.

#### Against the two per-path baselines

<img src="cross_library.png" width="640">

Linear for the shape, log for the detail — on a linear axis all three wingfoil
tiers are one line on the floor, which is the point being made and also why the
second chart exists. The log axis is the only place the ~55 ns between
`compiled` and the island is visible at all, and the only place you can see
rxrust starting *ahead* at depths 1–2:

<img src="cross_library_log.png" width="640">

| Depth | interpreted | compiled | island | rxrust | tokio async |
|---|---|---|---|---|---|
| 1  | 87.0 | 21.1 | 73.8 | 24 | 152 |
| 3  | 135.5 | 21.9 | 80.1 | 167 | 364 |
| 5  | 179.7 | 23.3 | 78.6 | 672 | 1 263 |
| 7  | 217.5 | 24.7 | 81.8 | 2 820 | 5 100 |
| 10 | 287.5 | 23.9 | 86.0 | 22 595 | 38 487 |
| *vs rxrust @ 10* | 78.6× | **945×** | 263× | 1× | — |
| *vs tokio @ 10* | 134× | **1610×** | 448× | — | 1× |

wingfoil stays flat while async streams and reactive double every level — 2.01×
and 1.94× per level measured over the sweep, O(2^N) per-path propagation. The
interpreter passes rxrust at **depth 3** and is ahead of tokio from **depth 1**;
the compiled tiers lead everything, everywhere. Extending the same slopes to
depth 20 puts the gap in the millions.

**Read the slopes; the ratios are indicative.** The wingfoil columns are per
*cycle*, from a graph driven by its own ticker. [`reactive.rs`](reactive.rs) and
[`async_streams.rs`](async_streams.rs) are per *source event*, called straight
into the library on the criterion thread. Neither side carries a bench
handshake, which is as close to like-for-like as these three targets get, but a
cycle and an event are not the same unit. The claim being made — linear against
doubling — is a statement about slopes and is untouched by that.

Two further caveats, both of which cut *against* wingfoil:

**The rxrust column is two source emissions per sample**, not one.
[`reactive.rs`](reactive.rs) calls `root.next` twice per `iter()` — the second
is what makes each level double, since `combine_latest(src, src)` emits once on
the first push and twice thereafter — where [`async_streams.rs`](async_streams.rs)'s
`block_on` evaluates one value. Per source event the rxrust ratios here are
about **half** what the table implies (~39× against the interpreter at depth 10,
not 78.6×). Left as the legacy target wrote it so the two readings stay
comparable; the slopes are untouched by the factor.

**`async_streams.rs` is not a stream, and barely a tokio measurement.** It is
recursive `Box::pin(async move …)` over an immediately-ready leaf, so nothing
ever yields and the scheduler is uninvolved past `block_on` — what doubles is
2^(N+1)−1 heap allocations plus poll dispatch, and `block_on`'s own per-sample
cost sits on top. It is an honest depiction of *per-path propagation*, which is
what the comparison is about, and it is not a claim about the best an async
implementation could do on this DAG.

Two readings of the same data, and they answer different questions. At depth 1
there is no branching to exploit and the gap is the *runtime* difference:
compiled 21.1 ns against tokio's 152 ns, **~7×**. At depth 10 the gap is the
*algorithmic* difference — O(N) against O(2^N) on a workload built to expose
it — and it is 1610×, unbounded in depth. Both are real; quote the one that
matches the claim being made, and halve the rxrust figures for a per-source-event
reading.

This is a *reading*, not source — measured on **machine B**, the 4-core 2.10 GHz
Xeon KVM guest described in [`../images/lscpu-b.txt`](../images/lscpu-b.txt), as
is the tier suite; the rest of [`../README.md`](../README.md) is still machine A.
Every series here was measured on B back to back, which is what makes them
comparable to each other and not to a table captured elsewhere. Regenerate
locally by running the three targets and refilling `plot.py` (the script's header
lists the commands) — the same script renders the headline pair the parent README
opens with, so both stay on one set of numbers. The legacy-engine plot, on the
same workload, is preserved here as
[`legacy_engine_latency.png`](legacy_engine_latency.png) — copied out of
`legacy/wingfoil/benches/bfs_vs_dfs/` ahead of the cutover, since it is the one
reading of this workload that cannot be regenerated once that tree is deleted.

### Why the difference?

**Per-path propagation (reactive / async):** when a source ticks, it fires
both arms of `combine_latest(src, src)` independently. Each arm triggers the
next level, which again fires both arms — 2^N callbacks or awaits across N
levels.

**Topological sort (wingfoil):** the graph is sorted so that every node is
scheduled after everything it reads, and the scheduler visits each node
exactly once per tick regardless of how many upstream paths lead to it. The
entire depth-127 graph in the
[topological_sort example](../../examples/core/topological_sort/) completes in
a single engine cycle.

### Benchmarks

| File | Framework | Pattern |
|------|-----------|---------|
| [wingfoil.rs](wingfoil.rs) | wingfoil | `s.join(&s, \|a, b\| a + b)` per level, one self-contained `nitro!` block per depth, run on all three tiers for a fixed cycle count |
| [async_streams.rs](async_streams.rs) | tokio async/await | recursive `branch_recombine` |
| [reactive.rs](reactive.rs) | rxrust 1.0 | `Subject` chain + `combine_latest` |

Only the first row changes across the port — the other two measure *other
libraries* as comparison baselines and are engine-agnostic, so their code is a
verbatim copy of the legacy files (only the wording of their header comments
differs). `wingfoil.rs` keeps the legacy workload node-for-node; legacy's free
function `add(&a, &b)` (a `bimap` with both upstreams active) is wingfoil's
`join`, one node per level either way. Its own module doc lists the rest of the
deviations — the `branch_recombine!` wiring macro, the `black_box` fences that
stop the compiled tiers collapsing 2^N additions of one value into a shift, and
the internal ticker that makes each graph self-contained.

### Running

The bench targets keep their historical names:

```bash
cargo bench --manifest-path crates/wingfoil/Cargo.toml --bench bfs_vs_dfs_wingfoil
cargo bench --manifest-path crates/wingfoil/Cargo.toml --bench bfs_vs_dfs_reactive
cargo bench --manifest-path crates/wingfoil/Cargo.toml --features async --bench bfs_vs_dfs_async_streams
```

The wingfoil target's groups are named `cycles_depth_1`..`cycles_depth_10`, one
per depth with an `interpreted` / `compiled` / `nested` bar in each. The prefix
stays even though there is no per-tick sweep left to distinguish them from: the
other two targets in this directory name their benchmarks `depth_1`..`depth_10`,
and all three write into the same `target/criterion/` tree, so a rename here
would collide with them. Filter with:

```bash
cargo bench --manifest-path crates/wingfoil/Cargo.toml --bench bfs_vs_dfs_wingfoil -- cycles_depth_10/
```
