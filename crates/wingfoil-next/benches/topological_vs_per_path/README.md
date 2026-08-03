## Topological Sort vs Per-Path Propagation: Branch / Recombine Benchmark

Port of `legacy/wingfoil/benches/bfs_vs_dfs/README.md` onto the next engine.

These benchmarks measure the cost of the branch/recombine pattern at depths 1–10:

<img src="diagram.png" width="200" align="centre">

At depth N the graph has 2^N paths from source to sink. The execution model
determines whether a framework pays O(N) or O(2^N) per tick.

### Results

<img src="latency.png" width="640">

wingfoil stays flat while async streams and reactive double every level
(O(2^N) per-path propagation). Point estimates, in nanoseconds per tick:

| Depth | wingfoil-next interpreted | wingfoil-next compiled island | rxrust (per-path) | tokio async streams (per-path) |
|---|---|---|---|---|
| 1  | 567 | 512 | 24 | 159 |
| 2  | 418 | 401 | 69 | 245 |
| 3  | 592 | 435 | 158 | 393 |
| 4  | 651 | 417 | 348 | 703 |
| 5  | 489 | 461 | 722 | 1 327 |
| 6  | 540 | 535 | 1 443 | 2 566 |
| 7  | 515 | 434 | 3 053 | 5 001 |
| 8  | 549 | 327 | 5 691 | 10 129 |
| 9  | 766 | 393 | 11 428 | 20 663 |
| 10 | 541 | 311 | 23 110 | 40 072 |

Both path-at-a-time libraries start out *ahead* — at depth 1 there is almost no
graph to schedule, and wingfoil is paying the bench harness's fixed handshake
(~450 ns of it; [see below](#the-graphs-own-cost-with-the-harness-divided-out),
and [`../README.md`](../README.md#graph-overhead) for the floor on its own). They
cross over by depth 5 (rxrust) and depth 4 (async streams), then double every
level, while both wingfoil tiers stay put: by depth 10 they are **~43× (rxrust)
and ~74× (async streams)** behind the interpreted engine. Extending the same
slopes to depth 20 puts the gap in the millions.

**The rxrust column is two source emissions per sample**, not one.
[`reactive.rs`](reactive.rs) calls `root.next` twice per `iter()` — the second
is what makes each level double, since `combine_latest(src, src)` emits once on
the first push and twice thereafter — where `add_bench`'s `step()` drives one
graph tick and [`async_streams.rs`](async_streams.rs)'s `block_on` evaluates one
value. Per source event the rxrust ratios here are about **half** what the table
implies (~21× at depth 10, not 43×). Left as the legacy target wrote it so the
two readings stay comparable; the slopes — linear against doubling, which is the
actual claim — are untouched by the factor.

The two wingfoil series are the same `nitro!` wiring on two engines — the
interpreted graph and the same DAG mounted as a single compiled island — and
neither trends with depth. **Do not read a per-depth figure off either of
them.** Most of each sample is the handshake, and the residue is noisy enough to
swamp the graph: the interpreted series wanders between 418 and 766 ns across a
sweep whose true cost (next section) moves by ~200 ns, and subtracting the two
harnesses depth by depth implies a "fixed" floor anywhere from 211 to 473 ns.
The island series is the faster of the two at every depth and flat within that
noise; it declines with depth, which more nodes cannot cause, so its downward
trend is an artefact and its minimum is not a measurement. The separation
between the tiers is quantified in the next section, where the handshake is
gone.

#### The graph's own cost, with the harness divided out

The `cycles_depth_N` groups run the identical wiring under a plain ticker for a
fixed 10 000 cycles, so no handshake sits under the measurement. Whole-run time
divided by the cycle count, in nanoseconds per cycle:

| Depth | Nodes | interpreted | compiled island |
|---|---|---|---|
| 1  | 4  | 122 | 84 |
| 2  | 5  | 142 | 86 |
| 3  | 6  | 166 | 85 |
| 4  | 7  | 184 | 87 |
| 5  | 8  | 206 | 86 |
| 6  | 9  | 225 | 92 |
| 7  | 10 | 249 | 88 |
| 8  | 11 | 272 | 91 |
| 9  | 12 | 293 | 90 |
| 10 | 13 | 330 | 90 |

<img src="per_cycle.png" width="640">

Three things this harness shows that the per-tick one cannot:

- **What the per-tick chart's flat line is made of.** At depth 1 the same graph
  costs 567 ns per tick and 122 ns per cycle, so ~450 ns of that sample is the
  criterion↔worker handshake, not the engine — consistent with
  [`../graph.rs`](../graph.rs)'s `node` bar, which wires no graph at all and
  measures the handshake on its own. Taking the same difference at every depth
  gives 211–473 ns rather than one number, so treat the floor as "several
  hundred nanoseconds, noisy" and not as a constant worth subtracting from
  individual samples. That floor is why the wingfoil series reads as flat well
  before the graph is; it is also why it is the *right* measurement for the
  cross-library comparison, which times rxrust and tokio the same way.
- **The O(N) claim, as a number.** Least squares over the ten depths puts the
  interpreted engine at **≈ 97 ns + 22 ns × depth**: one more level is one more
  node and a fixed ~22 ns, every tick. Across the sweep the path count grows
  512-fold (2 → 1024) while the cost grows 2.7×.
- **The island is flat outright** — 84 ns at depth 1, 90 ns at depth 10, a
  marginal **under 1 ns per level** (least squares over the ten depths gives
  0.7, against a 6 ns spread; read it as "flat", not as a figure good to one
  decimal). That is what a compiled interior is supposed to look like: the added
  node is one `u128` add behind a `__dirty[i]` predicate, emitted as
  monomorphized straight-line code, so depth costs about what the arithmetic
  costs and only the island's fixed boundary (a dyn call, the private queue, one
  outer activation) remains. It overtakes the interpreter at every depth, by
  1.5× at depth 1 and 3.7× at depth 10, and the gap keeps widening because only
  one of the two lines has a slope.

  Note the added work is genuinely *executed*, not optimized away: every level's
  sum passes through `black_box`, which [`wingfoil.rs`](wingfoil.rs)'s module doc
  puts there specifically so the compiled island cannot notice that summing one
  value 2^N times is a shift and collapse the whole chain. What the island
  removes is the *dispatch* around the add — the interpreter's ~22 ns of dyn
  call, `RefCell` borrow and slot clone — not the add.

  This line used to read ≈ 153 ns + 31 ns × depth — *steeper* than the
  interpreter's — because every inner node snapped its own `NanoTime::now()`
  (~24 ns, see the [`nanotime`](../README.md#the-clock) bench) when its
  `Ctx::nested` was built. The island now shares the outer cycle's wall snap. If
  you are comparing against a capture from before that fix, that is the whole
  difference.

This is a *reading*, not source — measured on **machine B**, the 4-core 2.10 GHz
Xeon KVM guest described in [`../images/lscpu-b.txt`](../images/lscpu-b.txt), as
is the tier suite; the rest of [`../README.md`](../README.md) is still machine A.
Every series here was measured on B back to back, which is what makes them
comparable to each other and not to a table captured elsewhere. Regenerate
locally by running the three targets and refilling `plot.py` (the script's header
lists the commands). The
legacy-engine plot, on the same workload, is preserved at
[`legacy/wingfoil/benches/bfs_vs_dfs/latency.png`](../../../../legacy/wingfoil/benches/bfs_vs_dfs/latency.png)
until the Phase-7 cutover.

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
| [wingfoil.rs](wingfoil.rs) | wingfoil-next | `s.join(&s, \|a, b\| a + b)` per level, one `nitro!` block per depth, run interpreted and as a compiled island |
| [async_streams.rs](async_streams.rs) | tokio async/await | recursive `branch_recombine` |
| [reactive.rs](reactive.rs) | rxrust 1.0 | `Subject` chain + `combine_latest` |

Only the first row changes across the port — the other two measure *other
libraries* as comparison baselines and are engine-agnostic, so their code is a
verbatim copy of the legacy files (only the wording of their header comments
differs). `wingfoil.rs` keeps the legacy workload node-for-node; legacy's free
function `add(&a, &b)` (a `bimap` with both upstreams active) is next's
`join`, one node per level either way. Its own module doc lists the rest of the
deviations — the `nitro!` unrolling, and the `black_box` fences that stop the
compiled island collapsing 2^N additions of one value into a shift.

### Running

The bench targets keep their historical names:

```bash
cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil
cargo bench -p wingfoil-next --bench bfs_vs_dfs_reactive
cargo bench -p wingfoil-next --features async --bench bfs_vs_dfs_async_streams
```

The wingfoil target runs both harnesses; criterion's filter picks one:

```bash
cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil -- 'depth_\d+(_nested)?$'
cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil -- cycles_
```
