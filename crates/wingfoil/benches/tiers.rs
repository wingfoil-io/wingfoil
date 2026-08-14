//! Phase-6 performance regression gate: benchmark the three `nitro!`-derived
//! execution tiers against one another on representative dispatch-heavy
//! workloads.
//!
//! One wiring definition per workload expands (via the `nitro!` macro) to all
//! three engines, which cannot drift because they share the same tokens and
//! `Op` semantics:
//! - `interpreted()` — the dynamic, shared-node engine (one dyn dispatch per
//!   node activation);
//! - `compiled()` — the fully monomorphized straight-line runner (the compiler
//!   optimizes across node boundaries);
//! - `nested()` — the compiled sub-graph mounted as a single island node of an
//!   interpreted graph (compiled-speed interior, one outer dyn call per
//!   activation).
//!
//! The regression thesis (see `docs/planning/port-plan.md`, Phase 6 + benchmarks):
//! `compiled` and `nested` should win on dense dispatch. This suite is the
//! scaffold that catches drift in that relationship; it grows as more ops reach
//! the compiled path.
//!
//! Workloads, each grouped so the tiers sit side by side:
//! - `dense_chain` — a deep linear map/filter/fold chain (dispatch-bound);
//! - `fanout` — the legacy 10x10 wide fan-out -> fan-in (shared wiring, every
//!   node fires every cycle);
//! - `fan_in_16` / `fan_in_64` / `fan_in_256` — a *busy* fan-in at three
//!   widths, all branches ticking every cycle. The width sweep is the point:
//!   `fanout` is only 10 wide, which is why it missed the n-ary-merge gap for
//!   as long as it did (see below);
//! - `accumulate` — a fold-accumulate hot loop over many cycles (scheduler-loop
//!   bound);
//! - `sparse` / `sparse_wide` — a hot chain in a graph that is ~97% quiet, at
//!   two padding widths. The three above are all dense, which is the compiled
//!   tier's home ground; these ask whether the ranking survives the regime the
//!   interpreted dirty-list was built for.
//!
//! Throughput is reported in node-cycles/sec (engine-cycles x node-count) so the
//! per-tier numbers are directly comparable across workloads.
//!
//! **There used to be a fourth bar.** `legacy` built the *same* workload on the
//! legacy `wingfoil` engine (the interpreted `Rc<dyn Stream>` node tree) as the
//! Phase-6 regression baseline, gating **wingfoil-interpreted ≥
//! legacy-interpreted**. That gate was met and the tree deleted at cutover, so
//! the bar is gone and the comparison cannot be re-run; the readings below are
//! its permanent record. What survives here is the tier-vs-tier comparison,
//! which is what the suite is for now.
//!
//! Measured relationship (relative, not absolute — the numbers move with
//! hardware): wingfoil-interpreted **met or beat** legacy on all three
//! workloads — the dispatch-bound `dense_chain`, the loop-bound `accumulate`,
//! and the wide `fanout` (every node fires every cycle). The compiled/nested
//! tiers win decisively across the board — the compiled fan-out runs ~37x faster
//! than wingfoil-interpreted (~53x vs legacy), the island ~8x. (An earlier `fanout` gap where
//! wingfoil-interpreted trailed legacy ~40% was the sparse dispatch's per-node
//! `BinaryHeap` push/pop; replacing it with legacy's layer-bucketed drain closed
//! it. A later capture had `nested` behind *interpreted* on all eight workloads,
//! which was `Ctx::nested` snapping a fresh `NanoTime::now()` per inner node per
//! activation — ~24 ns a node. This bench is the scaffold that keeps the
//! relationship honest, and it caught both.)
//!
//! **On the sparse workloads the ranking holds, but two things surface that the
//! dense groups hide.** Compiled still wins outright — the capture that first
//! established this read ~774us vs interpreted's ~2.94ms at 267 nodes, ~734us
//! vs ~3.18ms at 1035 — so there is no crossover where the dirty-list overtakes
//! straight-line emission, even at ~97% quiet: compiled's per-node `__dirty[i]`
//! predicate is cheap enough that walking a thousand of them costs less than
//! dispatching 8 dynamically. Sparse is also where `nested` is weakest, and for
//! the structural reason: the island runs its whole compiled interior on every
//! outer activation, so a mostly-quiet interior wastes most of it. It still
//! wins there (2.2x-2.8x) — where it used to *lose*, which turned out to be a
//! per-node `NanoTime::now()` in `Ctx::nested` rather than the design. The
//! island's genuinely thinnest margin is now `accumulate` at 1.0x: three nodes
//! give a composite almost nothing to amortise its boundary against.
//!
//! **The absolute figures in this module doc are the captures that motivated
//! each finding, not the current reading**, and they come from several runs on
//! different machines and different workload shapes (the node counts moved when
//! `fan` stopped left-folding into a merge chain). Read them as the evidence
//! for the *shape* of each claim; for numbers that are current and internally
//! comparable, see the table in
//! [`benches/README.md`](README.md#three-engines-one-wiring), which is refilled
//! as a whole group from one run.
//!
//! The second finding was the interpreted growth itself, and it led to two
//! fixes. 2.70ms -> 4.39ms for 4x the padding looked like a violation of "work
//! proportional to active nodes" and was not one: node count is genuinely free
//! (dangling padding of the same size costs nothing measurable). What grew was
//! *depth*. `fan` used to left-fold its branches into a binary merge chain, so
//! 256 branches was a ~256-deep graph, and the drain used to walk `0..=max_layer`
//! testing every bucket — `O(active + deepest active layer)` per cycle. Legacy
//! never showed it, its `merge(vec)` being one N-ary node (depth 1).
//!
//! The drain now scans an occupied-layer bitmask instead (64 layers per word
//! test), which took the slope between these two groups from +63% to +8%:
//! ~2.94ms and ~3.18ms.
//!
//! **The `fan_in_*` groups exist because chasing that depth term turned up its
//! root cause, and it was a parity gap rather than a tuning opportunity**: next
//! had no n-ary merge, so `merge_all`/`fan` cost `n - 1` merge nodes where
//! legacy's `merge(vec)` costs 1. On a *busy* fan-in — every branch ticking,
//! the common case — that was a straight loss against legacy that widened with
//! width (1.45x at 16, 1.73x at 64, 1.86x at 256), i.e. a violation of the
//! `wingfoil-interpreted >= legacy-interpreted` gate. `fanout` could not see it at
//! 10 wide: the 9 extra merge nodes were lost among ~105 others. Wingfoil now wires
//! a single [`MergeN`](wingfoil::ops::MergeN) node, and the node counts
//! below match the legacy twins exactly, which they did not before.
//!
//! Measured after the fix (10k cycles): interpreted 4.35ms / 13.08ms / 48.85ms
//! against legacy's 5.43ms / 16.76ms / 64.15ms — **0.80x / 0.78x / 0.76x**. The
//! flatness across the three widths is the thing to check, not any one bar: the
//! failure mode was a ratio that grew with width, so a regression reappears as a
//! rising slope long before any single group looks slow.
//!
//! See Phase 4.5 in `docs/planning/port-plan.md`.

use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);

// --- Workload 1: a deep linear map/filter/fold chain (dispatch-bound) -------
//
// count -> 32 unrolled maps (`map_n`) -> derive an even-ness predicate -> filter
// -> fold-sum. The straight-line chain is almost pure dispatch, so the compiled
// tier's cross-node optimization should show the largest win here.
wingfoil::nitro! {
    fn dense_chain(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let chained = src.map_n(32, |i: &u64| std::hint::black_box(i.wrapping_add(1)));
        let keep = chained.map(|i: &u64| i.is_multiple_of(2));
        let filtered = chained.filter(&keep);
        let sum = filtered.fold(0u64, |acc, v| *acc += v);
        sum
    }
}

// --- Workload 2: the legacy 10x10 wide fan-out -> fan-in -------------------
//
// Shared `nitro!` wiring, `include!`d so the bench and the `dual_mode` example
// measure the identical DAG shape. Defines module `fanout` with a top-level
// `const PERIOD`.
include!("../bench_support/fanout_10x10.rs");

// --- Workload 2b: a *busy* fan-in, swept across three widths ----------------
//
// One source feeds `width` one-map branches that all fan back into a single
// merge, so every branch ticks on every cycle. This is the shape the n-ary
// merge exists for, and the shape `fanout` is too narrow to measure: the cost
// of a fan-in's *merge* only separates from the cost of its *branches* once the
// width is large. Read the three groups as a slope — a merge chain regression
// shows up as a cost that grows faster than width, not as any single bad
// number.
wingfoil::nitro! {
    fn fan_in_16(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let out = src
            .fan(16, |s| s.map(|i: &u64| std::hint::black_box(i.wrapping_add(1))))
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

wingfoil::nitro! {
    fn fan_in_64(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let out = src
            .fan(64, |s| s.map(|i: &u64| std::hint::black_box(i.wrapping_add(1))))
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

wingfoil::nitro! {
    fn fan_in_256(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let out = src
            .fan(256, |s| s.map(|i: &u64| std::hint::black_box(i.wrapping_add(1))))
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

// --- Workload 3: a fold-accumulate hot loop (scheduler-loop bound) ----------
//
// Three nodes, but run for many cycles: the per-cycle scheduler overhead — not
// per-node dispatch — dominates. Guards against regressions in the run loop
// itself across the tiers.
wingfoil::nitro! {
    fn accumulate(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(STEP).count();
        let sum = count.fold(0u64, |acc, v| *acc += v);
        sum
    }
}

// --- Workload 4: a *sparse* graph — where the tiers may not rank as usual ---
//
// The three workloads above are dense: every node fires every cycle, which is
// exactly the regime the compiled tier is built for. But the interpreted
// engine's Phase 4.5 dirty-list buys something none of them can show — per-cycle
// work proportional to the *active* nodes rather than to `N` — and the compiled
// tier has no counterpart to it: `nitro!` emission is a straight-line walk of
// every node, each guarded by its own cheap `__dirty[i]` predicate, so a
// compiled cycle still pays an `O(N)` term over nodes that never fire.
//
// That suggests a crossover the dense workloads cannot see: as the quiet
// fraction of a graph grows, interpreted's cost stays pinned to the hot chain
// while compiled keeps walking the padding. This workload is where to look for
// it — one hot chain that fires every cycle, hung in a graph padded with cold
// branches on a driver whose period exceeds the run, so the padding is ~97% of
// the nodes and ~0% of the activity.
//
// **Measured: the crossover is not there at these sizes** — compiled wins by
// ~4x at 267 nodes and ~4x at 1035. The predicate walk is far cheaper per idle
// node than dynamic dispatch is per active one, so the quiet fraction would have
// to be enormous before the lines meet. `nested` is what inverts instead (it
// loses to interpreted here). See the module header for both findings.
//
// Read the bars *within* the group only: the throughput denominator counts all
// nodes (as the other groups do), but here most of them are deliberately idle,
// so the per-node rate is not comparable to the dense workloads.
// Two padding widths, because the interesting quantity is not either number but
// the *slope* between them: interpreted should be flat in the padding (its cost
// is pinned to the ~8 hot nodes) while compiled grows with it. Where those lines
// cross is the answer to "is compiled always the right tier?".
const COLD_PERIOD: Duration = Duration::from_millis(1);

wingfoil::nitro! {
    fn sparse(g: &GraphBuilder) -> Stream<u64> {
        let hot = g.ticker(STEP).count().map_n(6, |i: &u64| std::hint::black_box(i.wrapping_add(1)));
        let cold = g
            .ticker(COLD_PERIOD)
            .count()
            .fan(64, |s| s.map_n(3, |i: &u64| std::hint::black_box(i.wrapping_add(1))));
        let out = hot
            .merge(&cold)
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

// The same shape with 4x the padding (~1035 nodes, still ~8 of them active).
wingfoil::nitro! {
    fn sparse_wide(g: &GraphBuilder) -> Stream<u64> {
        let hot = g.ticker(STEP).count().map_n(6, |i: &u64| std::hint::black_box(i.wrapping_add(1)));
        let cold = g
            .ticker(COLD_PERIOD)
            .count()
            .fan(256, |s| s.map_n(3, |i: &u64| std::hint::black_box(i.wrapping_add(1))));
        let out = hot
            .merge(&cold)
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

/// Emit the tier comparison for one source-island workload: the three
/// `nitro!`-derived engines (`interpreted` / `compiled` / `nested`).
/// `$module` is the macro-generated wingfoil module, `$nodes` the node count
/// for the throughput label, and `$cycles` the fixed engine-cycle count.
macro_rules! tier_group {
    ($c:expr, $name:literal, $module:ident, $nodes:expr, $cycles:expr) => {{
        let run_for = RunFor::Cycles($cycles);
        let mut g = $c.benchmark_group($name);
        g.sample_size(20);
        g.throughput(Throughput::Elements($cycles as u64 * $nodes));

        // Graph *construction* is hoisted into `iter_batched`'s setup so only
        // dispatch is timed. It matters: wiring is `O(N)` while a sparse
        // workload's dispatch is `O(active)`, so on the `sparse_wide` group
        // (1035 nodes, ~8 active) build cost otherwise rivals the thing being
        // measured — and it lands on `interpreted`/`nested` but not
        // `compiled`, whose wiring is stack locals the compiler flattens. Timing
        // it would read as a compiled win that is really a construction
        // difference. The dense groups barely move either way.
        g.bench_function("interpreted", |b| {
            b.iter_batched(
                || $module::interpreted(),
                |(mut runner, out)| {
                    runner.run(HISTORICAL, run_for).unwrap();
                    black_box(runner.value(out))
                },
                BatchSize::SmallInput,
            )
        });

        // No setup to hoist: `compiled()` emits its wiring as straight-line
        // locals, so build and run are one generated function by construction.
        g.bench_function("compiled", |b| {
            b.iter(|| black_box($module::compiled(HISTORICAL, run_for).unwrap()))
        });

        g.bench_function("nested", |b| {
            b.iter_batched(
                || {
                    let gb = GraphBuilder::new();
                    let out = $module::nested(&gb);
                    (gb.build(), out)
                },
                |(mut runner, out)| {
                    runner.run(HISTORICAL, run_for).unwrap();
                    black_box(runner.value(&out))
                },
                BatchSize::SmallInput,
            )
        });

        g.finish();
    }};
}

fn tiers(c: &mut Criterion) {
    // dense_chain: ticker + count + 32 maps + even-map + filter + fold.
    tier_group!(c, "dense_chain", dense_chain, 37, 10_000u32);

    // fanout: ticker + count + 10*10 maps + the 10-way merge = 103, the same
    // count the legacy twin had once the fan-in became one n-ary node.
    tier_group!(c, "fanout", fanout, 10 * 10 + 3, 10_000u32);

    // fan_in_*: ticker + count + `width` maps + the n-ary merge + fold. Every
    // branch ticks every cycle, so this is the busy fan-in the n-ary merge
    // exists for; the three widths make a merge-chain regression visible as a
    // slope across the group.
    tier_group!(c, "fan_in_16", fan_in_16, 16 + 4, 10_000u32);
    tier_group!(c, "fan_in_64", fan_in_64, 64 + 4, 10_000u32);
    tier_group!(c, "fan_in_256", fan_in_256, 256 + 4, 10_000u32);

    // accumulate: ticker + count + fold, run long so the loop dominates.
    tier_group!(c, "accumulate", accumulate, 3, 20_000u32);

    // sparse: hot (ticker + count + 6 maps) + cold (ticker + count + 64*3 maps
    // + the n-ary merge) + the joining merge + fold = 205 nodes, ~8 of them
    // active. (Was 267 while `fan` unrolled to 63 binary merges.)
    tier_group!(c, "sparse", sparse, 205, 10_000u32);

    // sparse_wide: the same, with 256 cold branches — 781 nodes, ~8 active.
    tier_group!(c, "sparse_wide", sparse_wide, 781, 10_000u32);
}

criterion_group!(benches, tiers);
criterion_main!(benches);
