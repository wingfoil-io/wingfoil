## BFS vs DFS: Branch / Recombine Benchmark

These benchmarks measure the cost of the branch/recombine pattern at depths 1–10:

```
source → add(source, source) → add(…, …) → … (N levels)
```

At depth N the graph has 2^N paths from source to sink. The execution model
determines whether a framework pays O(N) or O(2^N) per tick.

### Results

| depth | wingfoil | async streams | reactive (rxrust) |
|------:|----------:|--------------:|------------------:|
|     1 |   174 ns  |      109 ns   |        66 ns      |
|     2 |   212 ns  |      165 ns   |       156 ns      |
|     3 |   197 ns  |      274 ns   |       324 ns      |
|     4 |   256 ns  |      545 ns   |       652 ns      |
|     5 |   264 ns  |     1.04 µs   |      1.35 µs      |
|     6 |   267 ns  |     2.05 µs   |      2.68 µs      |
|     7 |   287 ns  |     4.08 µs   |      5.35 µs      |
|     8 |   326 ns  |     8.10 µs   |     10.7  µs      |
|     9 |   301 ns  |    16.1  µs   |     22.5  µs      |
|    10 |   352 ns  |    32.1  µs   |     43.1  µs      |

Reactive and async streams double every level — clear O(2^N). Wingfoil stays
flat — O(N).  At depth 10 reactive is ~120× slower than wingfoil; at depth 20
it would be ~3 million times slower.

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
| [wingfoil.rs](wingfoil.rs) | wingfoil | `add(&src, &src)` via `add_bench` |
| [async_streams.rs](async_streams.rs) | tokio async/await | recursive `branch_recombine` |
| [reactive.rs](reactive.rs) | rxrust 1.0 | `Subject` chain + `combine_latest` |

### Running

```bash
cargo bench --bench bfs_vs_dfs_wingfoil --features async
cargo bench --bench bfs_vs_dfs_reactive
cargo bench --bench bfs_vs_dfs_async_streams --features async
```
