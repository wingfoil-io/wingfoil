# Benchmarks — wingfoil-next

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
| `bfs_vs_dfs_wingfoil` | `bench` | `bfs_vs_dfs_wingfoil` | branch/recombine at depths 1–10 on the next engine |
| `bfs_vs_dfs_reactive` | — | `bfs_vs_dfs_reactive` | the same pattern in rxrust (DFS comparison baseline) |
| `bfs_vs_dfs_async_streams` | `async` | `bfs_vs_dfs_async_streams` | the same pattern in tokio async/await (DFS comparison baseline) |
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

# breadth-first vs depth-first (see bfs_vs_dfs/README.md)
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

## Report output

`cargo bench` writes criterion's HTML report to `target/criterion/`. Legacy
captured one such report for its `10x10` graph run under
[`legacy/wingfoil/benches/images/`](../../../../wingfoil/benches/images/), with the
statistics table and plot commentary in
[`legacy/wingfoil/benches/README.md`](../../../../wingfoil/benches/README.md). Those are
*readings* on one specific machine, not source, so they are deliberately not
duplicated here — regenerate against your own hardware instead.
