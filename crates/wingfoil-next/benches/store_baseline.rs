//! Pre-arena baseline: the two measurements the arena / SoA value-store
//! decision hinges on (see `docs/port-plan.md`, Phase 4.5).
//!
//! The arena is a deferred perf follow-on. Before committing to it, we want the
//! go/no-go driven by numbers, not the ~1.1–1.5× estimate. This suite
//! establishes the interpreted-engine baseline so a future arena PR can show
//! its delta against these same graphs.
//!
//! Two workloads, each targeting a distinct question:
//!
//! 1. `sparse_dispatch` — the standing gate for the *already-landed* dirty-list
//!    scheduler: per-cycle work must track the *active* node count, not the
//!    graph size `N`. A tiny hot chain (fires every cycle) sits in a graph
//!    padded with many cold branches (slow tickers that almost never fire).
//!    `Dispatch::Sparse` (default) should be roughly flat as the cold padding
//!    grows; `Dispatch::FullSweep` (the retained `O(N)` oracle) should scale
//!    with it. The arena does *not* change this relationship — it's here as the
//!    perf-parity gate the plan owes, and as the sparse baseline.
//!
//! 2. `forward_clone` — the ceiling on what the arena + zero-copy passthrough
//!    could recover. A large payload is forwarded through a chain of `filter`
//!    hops; each hop republishes its input by **clone** today. Running the
//!    identical graph with a `Vec<u64>` payload (a deep 8 KiB memcpy per hop)
//!    versus an `Rc<Vec<u64>>` payload (a refcount bump per hop) brackets the
//!    cost: the `Vec` − `Rc<Vec>` gap is the clone tax that slot-aliasing would
//!    remove. If that gap is small, the aliasing work isn't worth it; if it's
//!    large, it is. Neither payload changes the result — only the copy cost.

use std::hint::black_box;
use std::rc::Rc;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use wingfoil_next::interp::Dispatch;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);
/// Cold-branch tickers fire this many times slower than the hot chain, so over
/// a bounded run they effectively never fire — they only pad the node count.
const COLD_STEP: Duration = Duration::from_millis(100);

// --- Workload 1: sparse dispatch — work should track active nodes, not N ----

/// One hot chain that fires every cycle, plus `cold` branches hung off slow
/// tickers that almost never fire. Returns the built runner and the hot output;
/// `dispatch` selects the sparse dirty-list (default) or the full-sweep oracle.
fn run_sparse(cold: usize, cycles: u32, dispatch: Dispatch) {
    let g = GraphBuilder::new();

    // Hot path: fires every cycle (~8 active nodes).
    let mut hot = g.ticker(STEP).count();
    for _ in 0..6 {
        hot = hot.map(|i: &u64| black_box(i.wrapping_add(1)));
    }
    let out = hot.fold(0u64, |acc, v| *acc = acc.wrapping_add(*v));

    // Cold padding: present in the graph (so FullSweep scans them) but seeded
    // only when due (so Sparse skips them) — the whole point of the contrast.
    for _ in 0..cold {
        let mut branch = g.ticker(COLD_STEP).count();
        for _ in 0..3 {
            branch = branch.map(|i: &u64| black_box(i.wrapping_add(1)));
        }
        black_box(&branch);
    }

    let mut runner = g.build().with_dispatch(dispatch);
    runner.run(HISTORICAL, RunFor::Cycles(cycles)).unwrap();
    black_box(runner.value(&out));
}

fn sparse_dispatch(c: &mut Criterion) {
    const CYCLES: u32 = 20_000;
    const COLD: usize = 256; // ~256*4 = 1024 cold nodes padding an ~8-node hot path

    let mut g = c.benchmark_group("sparse_dispatch");
    g.sample_size(20);
    g.throughput(Throughput::Elements(CYCLES as u64));

    // Default engine: work ∝ active nodes — should be near-flat vs the padding.
    g.bench_function("sparse", |b| {
        b.iter(|| run_sparse(COLD, CYCLES, Dispatch::Sparse))
    });
    // Retained O(N) oracle: work ∝ N — should pay for every cold node each cycle.
    g.bench_function("full_sweep", |b| {
        b.iter(|| run_sparse(COLD, CYCLES, Dispatch::FullSweep))
    });

    g.finish();
}

// --- Workload 2: large-payload forwarding clone — the aliasing ceiling -------

const VEC_LEN: usize = 1024; // 8 KiB per payload
const HOPS: usize = 16; // forwarding filters, one clone each per cycle
const FWD_CYCLES: u32 = 5_000;

// --- Workload 3: scalar forwarding — the aliasing *floor* --------------------
//
// The counterpoint to `forward_clone`. That workload brackets the *ceiling* of
// the arena+aliasing win on a pathological 8 KiB payload; this one brackets the
// *floor* — the common case where a wingfoil value is a scalar (`f64`, a small
// struct). Here a per-hop clone is a register copy, so the payload-copy axis
// that `forward_clone` measures has effectively collapsed: whatever this chain
// costs is dispatch, not copying, and aliasing the slot cannot recover it.
//
// Same graph shape as `forward_clone` (source → N forwarding `filter` hops →
// fold) so the numbers are directly comparable per hop. If `forward_scalar`
// lands near `forward_clone/rc_vec` (a refcount bump per hop — already
// copy-free), then scalar graphs have almost nothing for slot-aliasing to
// remove, and the arena's only remaining lever on them is SoA cache locality
// (not isolated here). The realistic per-graph win therefore lives *between*
// this floor and the `forward_clone` ceiling, weighted by how much payload a
// real graph actually forwards by clone.

fn forward_clone(c: &mut Criterion) {
    let mut g = c.benchmark_group("forward_clone");
    g.sample_size(20);
    // Throughput = clones performed on the forwarding path (cycles * hops).
    g.throughput(Throughput::Elements(FWD_CYCLES as u64 * HOPS as u64));

    // Owned Vec payload: each filter hop deep-copies the whole vector.
    g.bench_function("vec", |b| {
        b.iter(|| {
            let g = GraphBuilder::new();
            let src = g.ticker(STEP).count();
            let keep = src.map(|_: &u64| true);
            let mut cur = src.map(|i: &u64| vec![*i; VEC_LEN]);
            for _ in 0..HOPS {
                cur = cur.filter(&keep);
            }
            let out = cur.fold(0u64, |acc, v| *acc = acc.wrapping_add(v.len() as u64));
            let mut runner = g.build();
            runner.run(HISTORICAL, RunFor::Cycles(FWD_CYCLES)).unwrap();
            black_box(runner.value(&out));
        })
    });

    // Rc<Vec> payload: identical graph, but each hop's clone is a refcount bump.
    // The vec − rc gap is the clone tax slot-aliasing would eliminate.
    g.bench_function("rc_vec", |b| {
        b.iter(|| {
            let g = GraphBuilder::new();
            let src = g.ticker(STEP).count();
            let keep = src.map(|_: &u64| true);
            let mut cur = src.map(|i: &u64| Rc::new(vec![*i; VEC_LEN]));
            for _ in 0..HOPS {
                cur = cur.filter(&keep);
            }
            let out = cur.fold(0u64, |acc, v| *acc = acc.wrapping_add(v.len() as u64));
            let mut runner = g.build();
            runner.run(HISTORICAL, RunFor::Cycles(FWD_CYCLES)).unwrap();
            black_box(runner.value(&out));
        })
    });

    g.finish();
}

fn forward_scalar(c: &mut Criterion) {
    let mut g = c.benchmark_group("forward_scalar");
    g.sample_size(20);
    // Same per-hop throughput unit as forward_clone, for a direct comparison.
    g.throughput(Throughput::Elements(FWD_CYCLES as u64 * HOPS as u64));

    // f64 payload: each filter hop forwards its input by a register-cheap copy.
    // There is no vec/rc contrast to draw — a scalar clone is already free — so
    // the single number is the copy-free floor the arena cannot beat by aliasing.
    g.bench_function("f64", |b| {
        b.iter(|| {
            let g = GraphBuilder::new();
            let src = g.ticker(STEP).count();
            let keep = src.map(|_: &u64| true);
            let mut cur = src.map(|i: &u64| *i as f64);
            for _ in 0..HOPS {
                cur = cur.filter(&keep);
            }
            let out = cur.fold(0u64, |acc, v| *acc = acc.wrapping_add(*v as u64));
            let mut runner = g.build();
            runner.run(HISTORICAL, RunFor::Cycles(FWD_CYCLES)).unwrap();
            black_box(runner.value(&out));
        })
    });

    g.finish();
}

criterion_group!(benches, sparse_dispatch, forward_clone, forward_scalar);
criterion_main!(benches);
