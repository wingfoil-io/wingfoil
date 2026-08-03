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

Measured on two machines, because two sections have been re-captured since the
original run. Each section below says which one it came from; **numbers only
compare within a machine.**

| | Machine A | Machine B |
|---|---|---|
| CPU | Intel Xeon @ 2.80 GHz, 4 cores (KVM guest) | Intel Xeon @ 2.10 GHz, 4 cores (KVM guest) |
| Cache | L1d 128 KiB · L2 4 MiB · L3 33 MiB | L1d 192 KiB · L2 8 MiB · L3 260 MiB |
| `lscpu` | [`images/lscpu.txt`](images/lscpu.txt) | [`images/lscpu-b.txt`](images/lscpu-b.txt) |
| Sections | graph overhead, the clock, user ops, store baseline | [execution tiers](#execution-tiers), [topological sort](#topological-sort-vs-per-path-propagation) |

Toolchain either way: stable rustc, `bench` profile (`opt-level=3`), run with
`cargo bench -p wingfoil-next --features bench,async`. Legacy's reading, for
comparison, is in
[`legacy/wingfoil/benches/README.md`](../../../legacy/wingfoil/benches/README.md) — a
3.80 GHz Xeon, so its absolute numbers run faster than either.

**The machine-A sections predate the lazy wall-clock snap** (`Kernel::wall_time`
now resolves on first read in a cycle instead of being taken in `begin_cycle`).
That removes one `NanoTime::now()` — ~24 ns, see [the clock](#the-clock) — from
every cycle of every graph that never stamps latency, which is most of them, and
it bites hardest where the per-cycle cost is smallest. **`custom_op`'s two
compiled bars are the ones to distrust**: at ~60 ns per cycle they are the same
order as the read being removed. `legacy` is unaffected — it keeps its own eager
snap in `GraphState`, deliberately, since it is the regression control — and so
is [the clock](#the-clock), which times `NanoTime::now()` directly.

Those sections are left as captured rather than adjusted by hand; they need a
run on machine A. The two machine-B sections have been re-captured since the
change and do not carry this caveat.

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

| Workload | Nodes | legacy | interpreted | compiled | nested | interp/legacy | interp/nested |
|---|---|---|---|---|---|---|---|
| `dense_chain` | 37 | 7.9491 ms | 6.6387 ms | 187.08 µs | 930.81 µs | **0.84×** | 7.1× |
| `fanout` | 103 | 17.052 ms | 11.993 ms | 324.26 µs | 1.4309 ms | **0.70×** | 8.4× |
| `fan_in_16` | 20 | 4.7610 ms | 2.6683 ms | 174.44 µs | 854.77 µs | **0.56×** | 3.1× |
| `fan_in_64` | 68 | 10.722 ms | 6.4710 ms | 258.57 µs | 938.48 µs | **0.60×** | 6.9× |
| `fan_in_256` | 260 | 38.079 ms | 31.599 ms | 2.5496 ms | 3.0954 ms | **0.83×** | 10.2× |
| `accumulate` | 3 | 2.0837 ms | 1.4268 ms | 326.31 µs | 1.4033 ms | **0.68×** | 1.0× |
| `sparse` | 205 | 2.5036 ms | 2.1139 ms | 310.63 µs | 751.34 µs | **0.84×** | 2.8× |
| `sparse_wide` | 781 | 3.0716 ms | 2.0145 ms | 355.15 µs | 895.66 µs | **0.66×** | 2.2× |

Four things to read off it:

- **The Phase-6 gate holds on all eight workloads.** next-interpreted is
  0.56×–0.84× of legacy — at least as fast, as the plan requires, and by a
  wider margin than the previous capture (0.66×–1.00×).
- **Compiled wins everywhere**, from 4.4× faster than interpreted
  (`accumulate`, where the scheduler loop rather than dispatch dominates) up to
  37× (`fanout`, dense dispatch — its home ground).
- **So does nested**, by 2.2×–10.2× — except on `accumulate`, where at 1.0× it
  is now a wash. A three-node graph gives an island almost nothing to amortise
  its boundary against.
- **`fan_in_*` is no longer flat with width** — 0.56× / 0.60× / 0.83× at 16 /
  64 / 256, where the previous capture read 0.66× / 0.68× / 0.69×. Flatness is
  the actual check here, because the n-ary-merge regression this sweep was
  built to catch showed up as a ratio that *grew* with width. **Do not read
  this as that regression returning**: next-interpreted's own `fan_in_256` bar
  barely moved between captures (−0.6%), while *legacy's* fell 17.9% — the
  largest single move in the control column below. The ratio rose because the
  denominator moved. It is still worth a confirming re-run.

### What moved since the previous capture, and why

This capture is **not** a controlled before/after against the one it replaces:
"machine B" names a *spec*, and this is a different VM instance of it (same
model, caches and BogoMIPS as
[`images/lscpu-b.txt`](images/lscpu-b.txt), different host neighbours). The
`legacy` column is the useful control — no code in this repo changed under it —
so its movement measures instance-to-instance variation directly:

| Tier | Movement vs the previous capture | Code changed? |
|---|---|---|
| `legacy` | +3.7% … −17.9% | no — the control |
| `nested` | −7.5% … −30.8% | no (islands share one snap per activation either way) |
| `interpreted` | −0.6% … −31.1% | one clock read per cycle removed |
| `compiled` | −14.4% … −69.8% | one clock read per cycle removed |

Only **compiled** moves clearly outside the control's band, on seven of the
eight workloads — which is what you would expect from making the per-cycle wall
snap lazy: a ~24 ns clock read came off a bar that was running at ~55 ns per
cycle. The interpreted and nested columns overlap the control band, so their
movement is not attributable here, and this page does not attribute it.

An earlier capture had `nested` trailing `interpreted` on all eight workloads
(1.12×–1.43×), contradicting the bench's own module docs. That was a defect
rather than hardware: `Ctx::nested` snapped a fresh `NanoTime::now()` every
time it was built, i.e. **once per inner node per activation**, putting a ~24 ns
TSC read (see [the clock](#the-clock), which prices exactly that call) on every
node of every island. Islands now take the outer cycle's wall snap, which is
both faster and more correct — an island's ops agree with the rest of the graph
on what "this cycle" means instead of each reading its own instant.

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
interpreted engine (591 ns) is **~40× faster than rxrust** (23.705 µs) and
**~57× faster than tokio async streams** (33.750 µs); at depth 20 the same
slopes put the gap in the millions. The second wingfoil series is the same
`nitro!` wiring as a compiled island, also flat.

Two caveats on those multipliers, both of which cut *against* wingfoil:

- **The rxrust iteration is two source emissions** (`root.next` twice — the
  second is what produces the doubling), where `add_bench`'s `step()` and
  tokio's `block_on` are one each. Per source event the rxrust ratio is
  therefore about half the figure above, ~20× rather than 40×. The slopes,
  which are the actual claim, are unaffected.
- **Per-tick samples are mostly harness.** Do not read a single point off
  either wingfoil series: both are non-monotonic in depth (the island reads
  570 ns at depth 1 and 269 ns at depth 5, which added nodes cannot cause), and
  the interpreted series wanders between 323 and 683 ns across a sweep whose
  true cost moves by ~180 ns. The per-cycle table below is where the wingfoil
  result lives.

A second harness in the same target runs those graphs for a fixed 10 000 cycles
under a plain ticker, which divides the bench handshake out — several hundred ns
of every per-tick sample above — and turns the flat line into the actual scaling
law: **≈ 55 ns + 21.8 ns × depth** interpreted, one more node per level, while
the path count runs to 1024. The island is flat in the strong sense there —
**well under 1 ns per level**, 83 ns at depth 1 and 95 ns at depth 10 — because
its added node is a `u128` add and a `__dirty[i]` predicate of straight-line
monomorphized code, against the interpreter's ~22 ns of dyn dispatch, `RefCell`
borrow and slot clone. The interpreted slope is unchanged from the previous
capture (22 ns/level); its *fixed* term fell 97 → 55 ns, which is the per-cycle
wall-clock snap going lazy. (The adds themselves are not optimized away: every level's sum
passes through `black_box`, precisely so the compiled tier cannot collapse the
chain — see [`wingfoil.rs`](topological_vs_per_path/wingfoil.rs)'s module doc.)

Both sections were measured on machine B (see [above](#results)); everything
above them is machine A, and the two do not compare.
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
