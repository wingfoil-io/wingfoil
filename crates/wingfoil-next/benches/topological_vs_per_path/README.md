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
| 2  | 418 | 631 | 69 | 245 |
| 3  | 592 | 508 | 158 | 393 |
| 4  | 651 | 601 | 348 | 703 |
| 5  | 489 | 678 | 722 | 1 327 |
| 6  | 540 | 663 | 1 443 | 2 566 |
| 7  | 515 | 641 | 3 053 | 5 001 |
| 8  | 549 | 645 | 5 691 | 10 129 |
| 9  | 766 | 910 | 11 428 | 20 663 |
| 10 | 541 | 785 | 23 110 | 40 072 |

Both path-at-a-time libraries start out *ahead* — at depth 1 there is almost no
graph to schedule, and wingfoil is paying the bench harness's fixed handshake
(~450 ns of it; [see below](#the-graphs-own-cost-with-the-harness-divided-out),
and [`../README.md`](../README.md#graph-overhead) for the floor on its own).
They cross over by depth 5
(rxrust) and depth 4 (async streams), then double every level, while both
wingfoil tiers stay put: **43× (rxrust) and 74× (async streams)** behind by
depth 10. Extending the same slopes to depth 20 puts the gap in the millions.

The two wingfoil series are the same `nitro!` wiring on two engines — the
interpreted graph and the same DAG mounted as a single compiled island — and
neither trends with depth. They barely separate either, and at this resolution
they cannot: most of each sample is the harness. The island runs ~1.2× the
interpreted tier here, the same direction [`../tiers.rs`](../tiers.rs) measured
on its own workloads (`nested` trailing plain `interpreted` on all eight).

#### The graph's own cost, with the harness divided out

The `cycles_depth_N` groups run the identical wiring under a plain ticker for a
fixed 10 000 cycles, so no handshake sits under the measurement. Whole-run time
divided by the cycle count, in nanoseconds per cycle:

| Depth | Nodes | interpreted | compiled island |
|---|---|---|---|
| 1  | 4  | 122 | 178 |
| 2  | 5  | 142 | 206 |
| 3  | 6  | 166 | 251 |
| 4  | 7  | 184 | 288 |
| 5  | 8  | 206 | 313 |
| 6  | 9  | 225 | 356 |
| 7  | 10 | 249 | 373 |
| 8  | 11 | 272 | 398 |
| 9  | 12 | 293 | 439 |
| 10 | 13 | 330 | 458 |

<img src="per_cycle.png" width="640">

Three things this harness shows that the per-tick one cannot:

- **What the per-tick chart's flat line is made of.** At depth 1 the same graph
  costs 567 ns per tick and 122 ns per cycle, so ~450 ns of every sample in the
  table above is the criterion↔worker handshake, not the engine. That floor is
  why the wingfoil series reads as flat well before the graph is; it is also why
  it is the *right* measurement for the cross-library comparison, which times
  rxrust and tokio the same way.
- **The O(N) claim, as a number.** Least squares over the ten depths puts the
  interpreted engine at **≈ 97 ns + 22 ns × depth**: one more level is one more
  node and a fixed ~22 ns, every tick. Across the sweep the path count grows
  512-fold (2 → 1024) while the cost grows 2.7×.
- **The island does not pay off at this size** — ≈ 153 ns + 31 ns × depth, so it
  starts 56 ns behind and *widens*. Its interior is compiled straight-line code,
  but the boundary costs a dyn call plus per-activation bookkeeping, and twelve
  nodes of `u128` adds behind `black_box` fences are not enough work to earn
  that back. Consistent with the tier suite on this class of machine; worth
  re-checking on dedicated hardware before drawing a general conclusion, since
  the same suite's module docs expect islands to win on dense dispatch.

This is a *reading*, not source — measured on a 4-core 2.10 GHz Xeon KVM guest
([`../images/lscpu-topo.txt`](../images/lscpu-topo.txt)), which is **not** the
machine the rest of [`../README.md`](../README.md) was captured on: this section
alone was re-measured when the wingfoil target moved onto `nitro!`. Every series
in it was measured on that one machine, back to back, which is what makes them
comparable to each other — and not to the tables above. Regenerate it locally by
running the three targets and refilling `plot.py` (the script's header lists the
commands). The
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
