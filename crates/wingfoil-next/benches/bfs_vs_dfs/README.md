## Breadth First vs Depth First: Branch / Recombine Benchmark

Port of `legacy/wingfoil/benches/bfs_vs_dfs/README.md` onto the next engine.

These benchmarks measure the cost of the branch/recombine pattern at depths 1–10:

<img src="diagram.png" width="200" align="centre">

At depth N the graph has 2^N paths from source to sink. The execution model
determines whether a framework pays O(N) or O(2^N) per tick.

### Results

`latency.png` is a *reading*, not source, so it is not carried over from
legacy — regenerate it locally with `plot.py` after running the three targets
(the script's header lists the commands). The legacy-engine plot, on the same
workload, is preserved at
[`legacy/wingfoil/benches/bfs_vs_dfs/latency.png`](../../../../../wingfoil/benches/bfs_vs_dfs/latency.png)
until the Phase-7 cutover.

The shape it shows: wingfoil stays flat while async streams and reactive double
every level (O(2^N) DFS). At depth 10 both DFS approaches are ~120× slower than
wingfoil; at depth 20 that gap would be ~3 million×.

### Why the difference?

**Depth-first (reactive / async):** when a source ticks, it fires both arms of
`combine_latest(src, src)` independently. Each arm triggers the next level,
which again fires both arms — 2^N callbacks or awaits across N levels.

**Breadth-first (wingfoil):** the graph scheduler visits each node exactly
once per tick regardless of how many upstream paths lead to it. The entire
depth-127 graph in the [breadth_first example](../../examples/breadth_first/)
completes in a single engine cycle.

### Benchmarks

| File | Framework | Pattern |
|------|-----------|---------|
| [wingfoil.rs](wingfoil.rs) | wingfoil-next | `s.join(&s, \|a, b\| a + b)` via `add_bench` |
| [async_streams.rs](async_streams.rs) | tokio async/await | recursive `branch_recombine` |
| [reactive.rs](reactive.rs) | rxrust 1.0 | `Subject` chain + `combine_latest` |

Only the first row changes across the port — the other two measure *other
libraries* as comparison baselines and are engine-agnostic, so they are
verbatim copies of the legacy files. `wingfoil.rs` keeps the legacy workload
node-for-node; legacy's free function `add(&a, &b)` (a `bimap` with both
upstreams active) is next's `join`, one node per level either way.

### Running

```bash
cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil
cargo bench -p wingfoil-next --bench bfs_vs_dfs_reactive
cargo bench -p wingfoil-next --features async --bench bfs_vs_dfs_async_streams
```
