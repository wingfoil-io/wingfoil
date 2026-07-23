//! Phase-6 performance regression gate: benchmark the three `graph!`-derived
//! execution tiers against one another on representative dispatch-heavy
//! workloads.
//!
//! One wiring definition per workload expands (via the `graph!` macro) to all
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
//! Three workloads, each grouped so the tiers sit side by side:
//! - `dense_chain` — a deep linear map/filter/fold chain (dispatch-bound);
//! - `fanout` — the classic 10x10 wide fan-out -> fan-in (shared wiring, every
//!   node fires every cycle);
//! - `accumulate` — a fold-accumulate hot loop over many cycles (scheduler-loop
//!   bound).
//!
//! Throughput is reported in node-cycles/sec (engine-cycles x node-count) so the
//! per-tier numbers are directly comparable across workloads.

use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);

// --- Workload 1: a deep linear map/filter/fold chain (dispatch-bound) -------
//
// count -> 32 unrolled maps (`map_n`) -> derive an even-ness predicate -> filter
// -> fold-sum. The straight-line chain is almost pure dispatch, so the compiled
// tier's cross-node optimization should show the largest win here.
wingfoil_next::graph! {
    fn dense_chain(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(STEP).count();
        let chained = src.map_n(32, |i| std::hint::black_box(i.wrapping_add(1)));
        let keep = chained.map(|i| i.is_multiple_of(2));
        let filtered = chained.filter(&keep);
        let sum = filtered.fold(0u64, |acc, v| *acc += v);
        sum
    }
}

// --- Workload 2: the classic 10x10 wide fan-out -> fan-in -------------------
//
// Shared `graph!` wiring, `include!`d so the bench, the example, and the classic
// codegen suite all measure the identical DAG shape. Defines module `fanout`
// with a top-level `const PERIOD`.
include!("../bench_support/fanout_10x10.rs");

// --- Workload 3: a fold-accumulate hot loop (scheduler-loop bound) ----------
//
// Three nodes, but run for many cycles: the per-cycle scheduler overhead — not
// per-node dispatch — dominates. Guards against regressions in the run loop
// itself across the tiers.
wingfoil_next::graph! {
    fn accumulate(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(STEP).count();
        let sum = count.fold(0u64, |acc, v| *acc += v);
        sum
    }
}

/// Emit the three-tier `interpreted` / `compiled` / `nested` comparison for one
/// source-island workload (a `graph!` fn taking only the builder). `$module` is
/// the macro-generated module, `$nodes` the node count for the throughput label,
/// and `$cycles` the fixed engine-cycle count.
macro_rules! tier_group {
    ($c:expr, $name:literal, $module:ident, $nodes:expr, $cycles:expr) => {{
        let run_for = RunFor::Cycles($cycles);
        let mut g = $c.benchmark_group($name);
        g.sample_size(20);
        g.throughput(Throughput::Elements($cycles as u64 * $nodes));

        g.bench_function("interpreted", |b| {
            b.iter(|| {
                let (mut runner, out) = $module::interpreted();
                runner.run(HISTORICAL, run_for).unwrap();
                black_box(runner.value(out))
            })
        });

        g.bench_function("compiled", |b| {
            b.iter(|| black_box($module::compiled(HISTORICAL, run_for).unwrap()))
        });

        g.bench_function("nested", |b| {
            b.iter(|| {
                let gb = GraphBuilder::new();
                let out = $module::nested(&gb);
                let mut runner = gb.build();
                runner.run(HISTORICAL, run_for).unwrap();
                black_box(runner.value(&out))
            })
        });

        g.finish();
    }};
}

fn tiers(c: &mut Criterion) {
    // dense_chain: ticker + count + 32 maps + even-map + filter + fold.
    tier_group!(c, "dense_chain", dense_chain, 37, 10_000u32);

    // fanout: ticker + count + sample + fold + 10*10 maps + 10-way merge.
    tier_group!(c, "fanout", fanout, 10 * 10 + 5, 10_000u32);

    // accumulate: ticker + count + fold, run long so the loop dominates.
    tier_group!(c, "accumulate", accumulate, 3, 20_000u32);
}

criterion_group!(benches, tiers);
criterion_main!(benches);
