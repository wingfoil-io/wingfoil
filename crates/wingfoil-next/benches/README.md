# Benchmarks — wingfoil-next

**Jump to [Results](#results)** for a captured run — charts, statistics, and
what they say.

Criterion benchmarks for the next engine. Two groups live here:

- **next-specific** — the tier/engine suites that have no legacy counterpart
  (`tiers`, `custom_op`, `store_baseline`);
- **ports of `legacy/wingfoil/benches/`** — one target per legacy target, with the
  same name, the same `required-features` gating and, wherever possible, the
  same workload, so a next reading can be put straight beside the legacy one.
  That comparability is the whole point of the ports, and it disappears at the
  Phase-7 cutover when the legacy bar goes away.

**Benches are not a CI gate and are not meant to become one.** Criterion
wall-clock thresholds are too noisy on shared runners, so nothing in
`.github/workflows/` runs `cargo bench`; this suite is a run-on-demand
scaffold. The deterministic perf gates are *tests* — see
`tests/sparse_graph.rs` and `tests/merge_n.rs`, and `docs/port-plan.md`
("The perf gate — a test, not a benchmark").

## Targets

| Target | Features needed | Legacy twin | What it measures |
|---|---|---|---|
| `tiers` | — | *(next-only)* | legacy / interpreted / compiled / nested, side by side, on eight workloads |
| `custom_op` | — | *(next-only)* | a user op through the generic fallback vs a built-in table row, both compiled |
| `store_baseline` | — | *(next-only)* | the pre-arena baseline: sparse-vs-full-sweep dispatch, and the payload-clone ceiling/floor |
| `graph` | `bench` | `graph` | graph overhead: one engine cycle through a `width` × `depth` DAG |
| `nanotime` | — | `nanotime` | cost of reading the graph clock |
| `bfs_vs_dfs_wingfoil` | `bench` | `bfs_vs_dfs_wingfoil` | branch/recombine at depths 1–10 on the next engine, interpreted and as a compiled island |
| `bfs_vs_dfs_reactive` | — | `bfs_vs_dfs_reactive` | the same pattern in rxrust (per-path comparison baseline) |
| `bfs_vs_dfs_async_streams` | `async` | `bfs_vs_dfs_async_streams` | the same pattern in tokio async/await (per-path comparison baseline) |
| `iceoryx2` | `iceoryx2` | `iceoryx2` | `Burst<T>` push / iterate / clone |
| `iceoryx2_modes` | `iceoryx2` | `iceoryx2_modes` | the adapter's `Spin` / `Threaded` / `Signaled` subscriber modes |
| `aeron_publication_latency` | `aeron` | same | `offer` latency across message sizes |
| `aeron_subscription_throughput` | `aeron` | same | poll / poll-and-parse / burst throughput |
| `aeron_transceiver` | `aeron` | same | simultaneous pub+sub, request/response roundtrip, bidirectional exchange |
| `aeron_allocation_tracking` | `aeron` (+ `dhat-heap`) | same | allocations on the `offer` / `try_claim` / `poll` hot paths |

The `bench` feature exposes `wingfoil_next::bencher::add_bench`, the criterion
harness the graph benches drive — the twin of legacy's `bench`-gated
`bencher` module, and off by default for the same reason (criterion stays out
of a normal dependency tree).

## Running

```bash
# next-only suites
cargo bench -p wingfoil-next --bench tiers
cargo bench -p wingfoil-next --bench custom_op
cargo bench -p wingfoil-next --bench store_baseline

# graph overhead / clock
cargo bench -p wingfoil-next --features bench --bench graph
cargo bench -p wingfoil-next --bench nanotime

# topological sort vs per-path propagation (see topological_vs_per_path/README.md)
cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil
cargo bench -p wingfoil-next --bench bfs_vs_dfs_reactive
cargo bench -p wingfoil-next --features async --bench bfs_vs_dfs_async_streams

# adapters
cargo bench -p wingfoil-next --features iceoryx2 -- iceoryx2
cargo bench -p wingfoil-next --features aeron-driver --bench aeron_publication_latency
cargo bench -p wingfoil-next --features aeron-driver,dhat-heap --bench aeron_allocation_tracking
```

The aeron targets need a media driver: `--features aeron-driver` embeds one
in-process, or set `AERON_EXTERNAL_DRIVER=1` and run `aeronmd` yourself. With
only `--features aeron` (no driver) the benches print a skip message and exit
cleanly. They also need the Aeron C toolchain — clang, `uuid-dev`, CMake ≥ 3.20
(see the repo-root `CLAUDE.md`).

## What the ports changed, and what they did not

The rule for every port was: **keep the workload, change only what the engine
forces.** Each bench's own module doc records its deviations; in summary —

- `bfs_vs_dfs_reactive` and `bfs_vs_dfs_async_streams` benchmark *other
  libraries* (rxrust, tokio) as comparison baselines. They touch no wingfoil
  type at all, so they are verbatim copies.
- The four `aeron_*` benches and `iceoryx2` drive ported *backend* / value
  types (`RusteronPublisher`, `RusteronSubscriber`, `ClaimBuffer`, `Burst<T>`)
  rather than a graph, and those twins have identical signatures — so only the
  crate in the import path changes. `Burst<T>` and `NanoTime` are in fact the
  *same types* (next re-exports legacy's), so `iceoryx2` and `nanotime` measure
  identical code on both trees and must not diverge.
- `graph`, `bfs_vs_dfs_wingfoil` and `iceoryx2_modes` genuinely move onto the
  next engine. The rewiring is mechanical and node-count-preserving:
  `Rc<dyn Node>` factories become `GraphBuilder` + `Stream<T>`,
  `merge(vec)` becomes `merge_all` (one `MergeN` node either way),
  `add(&a, &b)` becomes `join` (one both-arms-active node either way),
  `produce(closure)` becomes `map(closure)`, and `map(f)` takes `&T` instead of
  `T`. Node counts match the legacy graphs exactly, so the numbers stay
  comparable.
- `bfs_vs_dfs_wingfoil` then goes one step further than a port: its depth sweep
  is defined in `nitro!` blocks, so each depth's wiring drives *two* engines
  (interpreted and a compiled island) instead of one, under two harnesses (per
  tick through the `add_bench` handshake, and per cycle with the handshake
  divided out). The legacy-comparable bar is still the interpreted, per-tick
  one, under the same `depth_N` names.

# Results

`cargo bench` writes criterion's HTML report to `target/criterion/`; the plots
below were lifted out of one such run by
[`../../../scripts/bench-report.sh`](../../../scripts/bench-report.sh), which
also prints the numbers in the tables. (The file format differs from legacy's
captured report: criterion 0.8 draws through the `plotters` backend and emits
**SVG**, where legacy's criterion 0.5 run used the gnuplot backend and emitted
PNG. Same plots, same statistics.)

**Everything below is a *reading*, not source.** Criterion wall-clock numbers
are hardware-specific, and this particular machine is a shared 4-core cloud VM
— noisier than legacy's reading in every respect (the `10x10` regression R²
below is 0.83, against 0.9999 on legacy's dedicated box). **Read the ratios,
not the absolute times**: every comparison here is between bars measured back
to back on the same machine in the same run, which is what the suite is for.

Measured on:

| | |
|---|---|
| CPU | Intel Xeon @ 2.80 GHz, 4 cores (KVM guest) |
| Cache | L1d 128 KiB · L2 4 MiB · L3 33 MiB |
| Toolchain | stable rustc, `bench` profile (`opt-level=3`) |
| Command | `cargo bench -p wingfoil-next --features bench,async` |

Full `lscpu` output: [`images/lscpu.txt`](images/lscpu.txt). Legacy's reading,
for comparison, is in
[`legacy/wingfoil/benches/README.md`](../../../legacy/wingfoil/benches/README.md) — a
3.80 GHz Xeon, so its absolute numbers run faster than these.

## Graph overhead

The [`graph`](graph.rs) bench wires a trivial DAG `width` × `depth`, with every
node ticking on every engine cycle, and measures one full engine cycle through
it.

| Bench | Nodes | Per engine cycle | Per node cycle |
|---|---|---|---|
| `node` | 0 (harness only) | 677.09 ns | — |
| `10x10` | 100 maps | **2.7090 µs** | 27.1 ns |
| `100x100` | 10 000 maps | 627.47 µs | 62.7 ns |

<img src="images/graph/10x10_pdf.svg" width="600">
<img src="images/graph/10x10_regression.svg" width="600">

The `node` row is the floor: it wires *no* graph at all, so it measures only
the criterion↔worker handshake the harness pays per sample (see
[`src/bencher.rs`](../src/bencher.rs)). Subtracting it puts the `10x10` graph
work at ~2.03 µs, i.e. ~20 ns per node cycle — so a 10-node graph would carry
roughly 200 ns of engine overhead per cycle. `100x100` costs ~2.3× more per
node than `10x10`: at 10 000 nodes the per-cycle working set no longer fits the
way a 100-node one does.

### Additional statistics — `10x10`

| Metric | Lower bound | Estimate | Upper bound |
|--------|------------|----------|------------|
| Slope  | 2.6820 µs  | 2.7090 µs | 2.7380 µs |
| R²     | 0.8245365  | 0.8328463 | 0.8232724 |
| Mean   | 2.6872 µs  | 2.7107 µs | 2.7352 µs |
| Std. Dev. | 102.00 ns | 122.94 ns | 141.79 ns |
| Median | 2.6453 µs  | 2.6828 µs | 2.7167 µs |
| MAD    | 83.996 ns  | 118.17 ns | 143.89 ns |

### Additional plots

- [Typical](images/graph/10x10_typical.svg)
- [Mean](images/graph/10x10_mean.svg)
- [Std. Dev.](images/graph/10x10_SD.svg)
- [Median](images/graph/10x10_median.svg)
- [MAD](images/graph/10x10_MAD.svg)
- [Slope](images/graph/10x10_slope.svg)
- [`node`](images/graph/node_pdf.svg) · [`100x100`](images/graph/100x100_pdf.svg)

### Understanding this report

The first plot displays the average time per iteration for this benchmark. The
shaded region shows the estimated probability of an iteration taking a certain
amount of time, while the line shows the mean.

The second plot shows the linear regression calculated from the measurements.
Each point represents a sample, though here it shows the total time for the
sample rather than time per iteration. The line is the line of best fit for
these measurements.

See the [Criterion.rs documentation](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html#additional-statistics)
for more detail on the additional statistics.

## The clock

[`nanotime`](nanotime.rs) — one `NanoTime::now()`: **24.340 ns**
([plot](images/graph/nanotime_pdf.svg)). `NanoTime` is a *shared* type (next
re-exports legacy's), so this bench measures identical code on both trees and
the two readings must not diverge.

## Execution tiers

[`tiers`](tiers.rs) runs each workload on four engines — the legacy
`MutableNode` engine as the regression baseline, and next's three
`nitro!`-derived tiers. Absolute times are for the whole fixed-cycle run
(10 000 cycles, 20 000 for `accumulate`).

<img src="images/tiers/summary.png" width="760">

| Workload | Nodes | legacy | interpreted | compiled | nested | interp/legacy |
|---|---|---|---|---|---|---|
| `dense_chain` | 37 | 9.3644 ms | 9.6772 ms | 905.05 µs | 10.999 ms | **1.03×** |
| `fanout` | 103 | 26.470 ms | 24.755 ms | 1.0067 ms | 29.091 ms | **0.94×** |
| `fan_in_16` | 20 | 5.5027 ms | 4.8737 ms | 537.14 µs | 6.3003 ms | **0.89×** |
| `fan_in_64` | 68 | 16.574 ms | 13.649 ms | 696.98 µs | 19.455 ms | **0.82×** |
| `fan_in_256` | 260 | 63.540 ms | 53.694 ms | 3.8365 ms | 74.364 ms | **0.85×** |
| `accumulate` | 3 | 2.7398 ms | 2.4097 ms | 1.0521 ms | 3.4333 ms | **0.88×** |
| `sparse` | 205 | 3.6242 ms | 3.2296 ms | 701.78 µs | 3.8995 ms | **0.89×** |
| `sparse_wide` | 781 | 4.0356 ms | 3.5344 ms | 783.85 µs | 3.9489 ms | **0.88×** |

Three things to read off it:

- **The Phase-6 gate holds on seven of eight workloads.** next-interpreted is
  0.82×–0.94× of legacy — at least as fast, as the plan requires. The
  exception is `dense_chain` at **1.03×**, ~3% behind legacy with the
  confidence intervals only just apart ([9.2638, 9.5371] ms legacy vs [9.5382,
  9.8624] ms interpreted). That is small enough to be this machine and large
  enough to be worth a re-run on a quiet box.
- **The `fan_in_*` ratios stay flat with width** — 0.89× / 0.82× / 0.85× at 16
  / 64 / 256. Flatness is the actual check: the n-ary-merge regression this
  sweep was built to catch showed up as a ratio that *grew* with width.
- **Compiled wins everywhere**, from 2.3× faster than interpreted
  (`accumulate`, where the scheduler loop rather than dispatch dominates) up to
  24.6× (`fanout`, dense dispatch — its home ground).

One divergence from the bench's own module docs, which record the compiled
*and* nested tiers winning on dense dispatch: **here `nested` trailed plain
`interpreted` on all eight workloads** (1.12×–1.43×), not just the sparse ones
where the docs already expect it to lose. Re-check that on dedicated hardware
before treating the module-doc ranking as current.

Per-workload violin plots:
[`dense_chain`](images/tiers/dense_chain.svg) ·
[`fanout`](images/tiers/fanout.svg) ·
[`fan_in_16`](images/tiers/fan_in_16.svg) ·
[`fan_in_64`](images/tiers/fan_in_64.svg) ·
[`fan_in_256`](images/tiers/fan_in_256.svg) ·
[`accumulate`](images/tiers/accumulate.svg) ·
[`sparse`](images/tiers/sparse.svg) ·
[`sparse_wide`](images/tiers/sparse_wide.svg)

## User ops vs the built-in table

[`custom_op`](custom_op.rs) — a 20-stage chain of a *user* op driven through
the generic compiled fallback, against the same chain built from a built-in
table row. 23 nodes × 10 000 cycles.

| Bench | Time | Throughput |
|---|---|---|
| `table_map_n_compiled` | 607.80 µs | 378.41 Melem/s |
| `custom_fallback_compiled` | 622.49 µs | 369.48 Melem/s |
| `custom_interpreted` | 5.5953 ms | 41.106 Melem/s |

A user op costs **2.4% more** than a built-in on the compiled path — the
generic fallback is not a second-class citizen, which is the property
`#[op(build = …)]` exists to guarantee. Both compiled paths are ~9× the
interpreted one. [Violin plot](images/ops/custom_op_dense_chain_20.svg).

## Store baseline

[`store_baseline`](store_baseline.rs) — the two measurements the arena / SoA
value-store decision hangs on.

**Sparse dispatch** (an ~8-node hot path in a graph padded to ~1030 nodes,
20 000 cycles): the dirty-list scheduler must track *active* nodes, not graph
size.

| Dispatch | Time | Per cycle |
|---|---|---|
| `Sparse` (default) | 19.676 ms | 984 ns |
| `FullSweep` (the `O(N)` oracle) | 121.08 ms | 6.05 µs |

**6.2× apart** — the dirty list is doing its job.
[Violin plot](images/ops/store_sparse_dispatch.svg).

**Payload clone tax** (a large payload forwarded through a chain of `filter`
hops, each republishing its input by clone):

| Payload | Time | Throughput |
|---|---|---|
| `Vec<u64>` (8 KiB deep copy per hop) | 22.346 ms | 3.5801 Melem/s |
| `Rc<Vec<u64>>` (refcount bump per hop) | 5.1688 ms | 15.478 Melem/s |
| `f64` (scalar floor) | 3.1081 ms | 25.739 Melem/s |

The `Vec` − `Rc<Vec>` gap is what slot-aliasing could recover: **4.3×**, or
17.2 ms of the 22.3 ms run. That is the ceiling on the zero-copy passthrough
work, and it is large. [Violin plot](images/ops/store_forward_clone.svg).

## Topological sort vs per-path propagation

[`topological_vs_per_path/`](topological_vs_per_path/) — the branch/recombine
pattern at depths 1–10, where each level doubles the number of source→sink
paths.

<img src="topological_vs_per_path/latency.png" width="640">

wingfoil-next stays flat across ten levels — every node visited once per tick —
while both path-at-a-time libraries double per level. At depth 10 the
interpreted engine (541 ns) is **43× faster than rxrust** (23.110 µs) and **74×
faster than tokio async streams** (40.072 µs); at depth 20 the same slopes put
the gap in the millions. The second wingfoil series is the same `nitro!` wiring
as a compiled island: also flat, ~1.2× the interpreted tier, the same direction
`nested` takes in the [tier suite](#execution-tiers) above.

A second harness in the same target runs those graphs for a fixed 10 000 cycles
under a plain ticker, which divides the bench handshake out — ~450 ns of every
per-tick sample above — and turns the flat line into the actual scaling law:
**≈ 97 ns + 22 ns × depth** interpreted, one more node per level, while the path
count runs to 1024.

**This section is a reading from a different machine** — a 4-core 2.10 GHz Xeon
VM ([`images/lscpu-topo.txt`](images/lscpu-topo.txt)), re-measured when the
wingfoil target moved onto `nitro!`, where everything above it came from the
2.80 GHz box. All four series in it were measured back to back on that one
machine, so they compare to each other; they do not compare to the tables above.
Full numbers and commentary:
[`topological_vs_per_path/README.md`](topological_vs_per_path/README.md).

## Not covered here

This run covered the targets that build under `--features bench,async`. The
`iceoryx2*` and `aeron_*` ones sit behind transport features that were not
enabled here — aeron additionally needs the Aeron C toolchain and a media
driver — so no reading is captured for them. Their commands are in
[Running](#running) above.

## Regenerating

```bash
scripts/bench-report.sh          # run the suite, refresh images/, print the tables
```

The script runs every target that needs no external service, copies criterion's
plots into [`images/`](images/), and prints each benchmark's estimate so the
tables above can be refilled. The two hand-drawn charts are rebuilt from data
pasted into their scripts: [`plot_tiers.py`](plot_tiers.py) (tier summary) and
[`topological_vs_per_path/plot.py`](topological_vs_per_path/plot.py)
(topological sort vs per-path propagation).
