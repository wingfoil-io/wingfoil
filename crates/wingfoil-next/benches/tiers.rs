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
//! The regression thesis (see `docs/port-plan.md`, Phase 6 + benchmarks):
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
//! A fourth bar, `legacy`, builds the *same* workload on the legacy `wingfoil`
//! engine (the interpreted `Rc<dyn Stream>` node tree) — the Phase-6 regression
//! baseline. The plan's goal is **next-interpreted ≥ legacy-interpreted** (no
//! slower); running `cargo bench --bench tiers` puts `legacy` and `interpreted`
//! side by side per workload so that relationship is directly readable.
//!
//! Measured relationship (relative, not absolute — the numbers move with
//! hardware): next-interpreted **meets or beats** legacy on all three
//! workloads — the dispatch-bound `dense_chain`, the loop-bound `accumulate`,
//! and the wide `fanout` (every node fires every cycle). The compiled/nested
//! tiers win decisively across the board — the compiled fan-out runs ~25x faster
//! than either interpreter. (An earlier `fanout` gap where next-interpreted
//! trailed legacy ~40% was the sparse dispatch's per-node `BinaryHeap`
//! push/pop; replacing it with legacy's layer-bucketed drain closed it. This
//! bench is the scaffold that keeps the relationship honest.)
//!
//! **On the sparse workloads the ranking holds, but two things surface that the
//! dense groups hide.** Compiled still wins outright — ~774us vs interpreted's
//! ~2.94ms at 267 nodes, ~734us vs ~3.18ms at 1035 — so there is no crossover
//! where the dirty-list overtakes straight-line emission, even at ~97% quiet:
//! compiled's per-node `__dirty[i]` predicate is cheap enough that walking 1035
//! of them costs less than dispatching 8 dynamically. What does invert is
//! `nested`, which *loses to plain interpreted* here (~3.66ms vs ~2.94ms) — the
//! island runs its whole compiled interior on every outer activation, so a
//! mostly-quiet interior is exactly its worst case, the mirror image of the
//! dense wins.
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
//! `next-interpreted >= legacy-interpreted` gate. `fanout` could not see it at
//! 10 wide: the 9 extra merge nodes were lost among ~105 others. next now wires
//! a single [`MergeN`](wingfoil_next::ops::MergeN) node, and the node counts
//! below match the legacy twins exactly, which they did not before.
//!
//! Measured after the fix (10k cycles): interpreted 4.35ms / 13.08ms / 48.85ms
//! against legacy's 5.43ms / 16.76ms / 64.15ms — **0.80x / 0.78x / 0.76x**. The
//! flatness across the three widths is the thing to check, not any one bar: the
//! failure mode was a ratio that grew with width, so a regression reappears as a
//! rising slope long before any single group looks slow.
//!
//! See Phase 4.5 in `docs/port-plan.md`.

use std::rc::Rc;
use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil_next::{NanoTime, RunFor, RunMode};
// Legacy-engine ops for the `legacy` baseline. Imported as `_` where only the
// trait methods are needed, so the names don't collide with the next prelude.
use wingfoil::{
    NodeOperators as _, StreamOperators as _, merge as legacy_merge, ticker as legacy_ticker,
};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);

// --- Workload 1: a deep linear map/filter/fold chain (dispatch-bound) -------
//
// count -> 32 unrolled maps (`map_n`) -> derive an even-ness predicate -> filter
// -> fold-sum. The straight-line chain is almost pure dispatch, so the compiled
// tier's cross-node optimization should show the largest win here.
wingfoil_next::nitro! {
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
// Shared `nitro!` wiring, `include!`d so the bench, the example, and the legacy
// codegen suite all measure the identical DAG shape. Defines module `fanout`
// with a top-level `const PERIOD`.
include!("../bench_support/fanout_10x10.rs");

// --- Workload 2b: a *busy* fan-in, swept across three widths ----------------
//
// One source feeds `width` one-map branches that all fan back into a single
// merge, so every branch ticks on every cycle. This is the shape the n-ary
// merge exists for, and the shape `fanout` is too narrow to measure: the cost
// of a fan-in's *merge* only separates from the cost of its *branches* once the
// width is large. Compare the `interpreted` and `legacy` bars within each
// group, then read the three groups as a slope — a merge chain regression shows
// up as a ratio that grows with width, not as any single bad number.
wingfoil_next::nitro! {
    fn fan_in_16(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let out = src
            .fan(16, |s| s.map(|i: &u64| std::hint::black_box(i.wrapping_add(1))))
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

wingfoil_next::nitro! {
    fn fan_in_64(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let out = src
            .fan(64, |s| s.map(|i: &u64| std::hint::black_box(i.wrapping_add(1))))
            .fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));
        out
    }
}

wingfoil_next::nitro! {
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
wingfoil_next::nitro! {
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

wingfoil_next::nitro! {
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
wingfoil_next::nitro! {
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

// --- Legacy-engine twins of the three workloads (the `legacy` baseline) -----
//
// Each builds the *same* DAG shape on the legacy `wingfoil` interpreted engine.
// `map_n`/`fan` are next-only sugar, so the repetition is spelled out with plain
// loops here; the node counts (and thus the throughput denominators) match.

fn dense_chain_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    let mut chained = legacy_ticker(STEP).count();
    for _ in 0..32 {
        chained = chained.map(|i: u64| std::hint::black_box(i.wrapping_add(1)));
    }
    let keep = chained.map(|i: u64| i.is_multiple_of(2));
    let filtered = chained.filter(keep);
    filtered.fold(|acc: &mut u64, v: u64| *acc += v)
}

fn fanout_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    let src = legacy_ticker(PERIOD).count();
    let branches = (0..10)
        .map(|_| {
            let mut s = src.clone();
            for _ in 0..10 {
                s = s.map(|i: u64| std::hint::black_box(i));
            }
            s
        })
        .collect::<Vec<_>>();
    legacy_merge(branches)
}

fn fan_in_legacy_n(width: usize) -> Rc<dyn wingfoil::Stream<u64>> {
    let src = legacy_ticker(STEP).count();
    let branches = (0..width)
        .map(|_| {
            src.clone()
                .map(|i: u64| std::hint::black_box(i.wrapping_add(1)))
        })
        .collect::<Vec<_>>();
    legacy_merge(branches).fold(|acc: &mut u64, v: u64| *acc = acc.wrapping_add(v))
}

fn fan_in_16_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    fan_in_legacy_n(16)
}

fn fan_in_64_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    fan_in_legacy_n(64)
}

fn fan_in_256_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    fan_in_legacy_n(256)
}

fn accumulate_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    legacy_ticker(STEP)
        .count()
        .fold(|acc: &mut u64, v: u64| *acc += v)
}

fn sparse_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    sparse_legacy_n(64)
}

fn sparse_wide_legacy() -> Rc<dyn wingfoil::Stream<u64>> {
    sparse_legacy_n(256)
}

fn sparse_legacy_n(cold_branches: usize) -> Rc<dyn wingfoil::Stream<u64>> {
    let mut hot = legacy_ticker(STEP).count();
    for _ in 0..6 {
        hot = hot.map(|i: u64| std::hint::black_box(i.wrapping_add(1)));
    }
    let cold_src = legacy_ticker(COLD_PERIOD).count();
    let cold = legacy_merge(
        (0..cold_branches)
            .map(|_| {
                let mut s = cold_src.clone();
                for _ in 0..3 {
                    s = s.map(|i: u64| std::hint::black_box(i.wrapping_add(1)));
                }
                s
            })
            .collect::<Vec<_>>(),
    );
    legacy_merge(vec![hot, cold]).fold(|acc: &mut u64, v: u64| *acc = acc.wrapping_add(v))
}

/// Emit the four-tier comparison for one source-island workload: the legacy
/// baseline plus the three `nitro!`-derived engines (`interpreted` / `compiled`
/// / `nested`). `$module` is the macro-generated next module, `$legacy` a
/// zero-arg builder of the legacy-engine twin, `$nodes` the node count for the
/// throughput label, and `$cycles` the fixed engine-cycle count.
macro_rules! tier_group {
    ($c:expr, $name:literal, $module:ident, $legacy:expr, $nodes:expr, $cycles:expr) => {{
        let run_for = RunFor::Cycles($cycles);
        let mut g = $c.benchmark_group($name);
        g.sample_size(20);
        g.throughput(Throughput::Elements($cycles as u64 * $nodes));

        // Graph *construction* is hoisted into `iter_batched`'s setup so only
        // dispatch is timed. It matters: wiring is `O(N)` while a sparse
        // workload's dispatch is `O(active)`, so on the `sparse_wide` group
        // (1035 nodes, ~8 active) build cost otherwise rivals the thing being
        // measured — and it lands on `legacy`/`interpreted`/`nested` but not
        // `compiled`, whose wiring is stack locals the compiler flattens. Timing
        // it would read as a compiled win that is really a construction
        // difference. The dense groups barely move either way.
        //
        // Phase-6 regression baseline: the legacy interpreted engine.
        g.bench_function("legacy", |b| {
            b.iter_batched(
                $legacy,
                |out| {
                    out.run(HISTORICAL, run_for).unwrap();
                    black_box(out.peek_value())
                },
                BatchSize::SmallInput,
            )
        });

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
    tier_group!(
        c,
        "dense_chain",
        dense_chain,
        dense_chain_legacy,
        37,
        10_000u32
    );

    // fanout: ticker + count + 10*10 maps + the 10-way merge = 103, the same
    // count as the legacy twin now that the fan-in is one n-ary node.
    tier_group!(c, "fanout", fanout, fanout_legacy, 10 * 10 + 3, 10_000u32);

    // fan_in_*: ticker + count + `width` maps + the n-ary merge + fold. Every
    // branch ticks every cycle, so this is the busy fan-in the n-ary merge
    // exists for; the three widths make a merge-chain regression visible as a
    // slope against `legacy`.
    tier_group!(
        c,
        "fan_in_16",
        fan_in_16,
        fan_in_16_legacy,
        16 + 4,
        10_000u32
    );
    tier_group!(
        c,
        "fan_in_64",
        fan_in_64,
        fan_in_64_legacy,
        64 + 4,
        10_000u32
    );
    tier_group!(
        c,
        "fan_in_256",
        fan_in_256,
        fan_in_256_legacy,
        256 + 4,
        10_000u32
    );

    // accumulate: ticker + count + fold, run long so the loop dominates.
    tier_group!(c, "accumulate", accumulate, accumulate_legacy, 3, 20_000u32);

    // sparse: hot (ticker + count + 6 maps) + cold (ticker + count + 64*3 maps
    // + the n-ary merge) + the joining merge + fold = 205 nodes, ~8 of them
    // active. (Was 267 while `fan` unrolled to 63 binary merges.)
    tier_group!(c, "sparse", sparse, sparse_legacy, 205, 10_000u32);

    // sparse_wide: the same, with 256 cold branches — 781 nodes, ~8 active.
    tier_group!(
        c,
        "sparse_wide",
        sparse_wide,
        sparse_wide_legacy,
        781,
        10_000u32
    );
}

criterion_group!(benches, tiers);
criterion_main!(benches);
