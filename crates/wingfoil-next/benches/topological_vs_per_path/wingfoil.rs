//! Branch/recombine at depths 1–10 on the wingfoil-next engine — the
//! topological-sort arm of the comparison against per-path propagation.
//!
//! Port of legacy `legacy/wingfoil/benches/bfs_vs_dfs/wingfoil.rs`. The workload is
//! unchanged: at depth N the graph has 2^N source→sink paths, and a
//! topologically sorted scheduler visits each node exactly once per tick, so
//! the measured cost should stay flat as the depth grows (see
//! `benches/topological_vs_per_path/README.md`).
//!
//! Each depth is wired **once**, in a `nitro!` block, and measured on two
//! engines derived from those same tokens:
//!
//! - `wire()` — the interpreted engine (the tier legacy measures, and the one
//!   the cross-library comparison is about);
//! - `nested()` — the same DAG mounted as a single compiled island: a
//!   monomorphized straight-line interior, one dyn call at the boundary per
//!   activation.
//!
//! Both tiers run under two harnesses, which answer different questions:
//!
//! - `depth_N` / `depth_N_nested` — one tick end to end through the `add_bench`
//!   handshake, the measurement the rxrust and tokio targets also make, and so
//!   the only one comparable across libraries;
//! - `cycles_depth_N/{interpreted,nested}` — the same wiring driven by a plain
//!   ticker for a fixed 10 000 cycles, which divides the harness out and leaves
//!   the graph's own per-tick cost (see the comment on that group).
//!
//! There is no `compiled()` bar. The harness drives the graph through a trigger
//! stream, so these are *input-taking* graphs, which `nitro!` deliberately
//! emits as components (`wire` + `nested`) and never as standalone programs —
//! `compiled()` owns its own run loop and has no way to accept the bencher's
//! handshake source. `nested()` is the compiled tier available to any graph fed
//! from outside, which is every graph with live inputs.
//!
//! The sweep is ten `nitro!` blocks rather than a loop over `depth`, because
//! `nitro!` requires straight-line wiring: nodes built inside a `for` loop are
//! invisible to the compiled emission. A static topology is the premise of the
//! whole comparison, so spelling it out is not much of a price.
//!
//! Deliberate deviations from the legacy source, all mechanical:
//!
//! - the builder closure takes `(&GraphBuilder, &Stream<()>)` and returns an
//!   [`Upstream`] instead of `Rc<dyn Node> -> Rc<dyn Node>`, because next wires
//!   nodes through a builder (see `wingfoil_next::bencher`);
//! - legacy's free function `add(&s, &s)` — a `bimap` with *both* upstreams
//!   active — is spelled `s.join(&s, |a, b| a + b)` here, next's two-active-input
//!   combine. Same node, same both-arms-active shape, same one node per level;
//! - every level's sum passes through [`std::hint::black_box`], and so does the
//!   source value, which legacy does not need. Without the fences the compiled
//!   island is free to notice that summing one value 2^N times is a shift, and
//!   to collapse the whole chain — turning the island bars into a measurement
//!   of nothing. They cost the interpreted tier nothing measurable (dispatch
//!   dominates there), so both tiers stay honest; `benches/tiers.rs` fences its
//!   map closures the same way;
//! - the source is legacy's `count()` mapped to a `u128`, but it forwards the
//!   count rather than legacy's constant `1`, so the value entering the stack
//!   changes every tick and nothing downstream can be hoisted out of the run
//!   loop. One map node either way.
//!
//! Run with: `cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil`

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use wingfoil_next::bencher::add_bench;
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

// One diamond per level: each `join` reads the *same* upstream twice, so at
// depth N the graph has 2^N source→sink paths across N + 2 nodes.

wingfoil_next::nitro! {
    fn depth_1(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        s1
    }
}

wingfoil_next::nitro! {
    fn depth_2(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        s2
    }
}

wingfoil_next::nitro! {
    fn depth_3(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        s3
    }
}

wingfoil_next::nitro! {
    fn depth_4(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        s4
    }
}

wingfoil_next::nitro! {
    fn depth_5(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        let s5 = s4.join(&s4, |a: &u128, b: &u128| black_box(a + b));
        s5
    }
}

wingfoil_next::nitro! {
    fn depth_6(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        let s5 = s4.join(&s4, |a: &u128, b: &u128| black_box(a + b));
        let s6 = s5.join(&s5, |a: &u128, b: &u128| black_box(a + b));
        s6
    }
}

wingfoil_next::nitro! {
    fn depth_7(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        let s5 = s4.join(&s4, |a: &u128, b: &u128| black_box(a + b));
        let s6 = s5.join(&s5, |a: &u128, b: &u128| black_box(a + b));
        let s7 = s6.join(&s6, |a: &u128, b: &u128| black_box(a + b));
        s7
    }
}

wingfoil_next::nitro! {
    fn depth_8(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        let s5 = s4.join(&s4, |a: &u128, b: &u128| black_box(a + b));
        let s6 = s5.join(&s5, |a: &u128, b: &u128| black_box(a + b));
        let s7 = s6.join(&s6, |a: &u128, b: &u128| black_box(a + b));
        let s8 = s7.join(&s7, |a: &u128, b: &u128| black_box(a + b));
        s8
    }
}

wingfoil_next::nitro! {
    fn depth_9(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        let s5 = s4.join(&s4, |a: &u128, b: &u128| black_box(a + b));
        let s6 = s5.join(&s5, |a: &u128, b: &u128| black_box(a + b));
        let s7 = s6.join(&s6, |a: &u128, b: &u128| black_box(a + b));
        let s8 = s7.join(&s7, |a: &u128, b: &u128| black_box(a + b));
        let s9 = s8.join(&s8, |a: &u128, b: &u128| black_box(a + b));
        s9
    }
}

wingfoil_next::nitro! {
    fn depth_10(_g: &GraphBuilder, trig: &Stream<()>) -> Stream<u128> {
        let s0 = trig.count().map(|c: &u64| black_box(*c as u128));
        let s1 = s0.join(&s0, |a: &u128, b: &u128| black_box(a + b));
        let s2 = s1.join(&s1, |a: &u128, b: &u128| black_box(a + b));
        let s3 = s2.join(&s2, |a: &u128, b: &u128| black_box(a + b));
        let s4 = s3.join(&s3, |a: &u128, b: &u128| black_box(a + b));
        let s5 = s4.join(&s4, |a: &u128, b: &u128| black_box(a + b));
        let s6 = s5.join(&s5, |a: &u128, b: &u128| black_box(a + b));
        let s7 = s6.join(&s6, |a: &u128, b: &u128| black_box(a + b));
        let s8 = s7.join(&s7, |a: &u128, b: &u128| black_box(a + b));
        let s9 = s8.join(&s8, |a: &u128, b: &u128| black_box(a + b));
        let s10 = s9.join(&s9, |a: &u128, b: &u128| black_box(a + b));
        s10
    }
}

/// Both tiers of one depth: the interpreted graph under the legacy bench name
/// `depth_N`, then the compiled island as `depth_N_nested`.
macro_rules! bench_depth {
    ($crit:expr, $module:ident) => {{
        add_bench(
            $crit,
            stringify!($module),
            |g: &GraphBuilder, trig: &Stream<()>| $module::wire(g, trig).upstream(),
        );
        add_bench(
            $crit,
            concat!(stringify!($module), "_nested"),
            |g: &GraphBuilder, trig: &Stream<()>| $module::nested(g, trig).upstream(),
        );
    }};
}

fn latency(crit: &mut Criterion) {
    bench_depth!(crit, depth_1);
    bench_depth!(crit, depth_2);
    bench_depth!(crit, depth_3);
    bench_depth!(crit, depth_4);
    bench_depth!(crit, depth_5);
    bench_depth!(crit, depth_6);
    bench_depth!(crit, depth_7);
    bench_depth!(crit, depth_8);
    bench_depth!(crit, depth_9);
    bench_depth!(crit, depth_10);
}

// --- The same sweep without the harness floor -------------------------------
//
// `add_bench` measures one tick end to end, which is what makes it comparable
// with the rxrust and tokio targets — they are timed the same way. The price is
// a criterion↔worker handshake under every sample (~450 ns on the captured run,
// read off the gap between the two harnesses at depth 1; `../graph.rs`'s `node`
// bench measures the same floor in isolation), and a twelve-node graph costs a
// fraction of that. The sweep therefore reads as flat long before the *graph* is
// flat, and the two tiers barely separate.
//
// These groups run the identical `nitro!` wiring under a plain ticker for a
// fixed number of cycles instead: no handshake, no spin loop, whole-run time
// divided by `CYCLES` is the per-tick cost of the graph itself. Construction is
// hoisted into `iter_batched`'s setup so only dispatch is timed, as in
// `../tiers.rs`. Read the `interpreted`/`nested` bars against each other within
// a group, and the groups against each other as a slope in depth: this is where
// "each node once per tick" turns into a number — an added level is one more
// node, not twice the work.
const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);
const CYCLES: u32 = 10_000;

/// One fixed-cycle group per depth, `interpreted` beside `nested`. `$nodes` is
/// the driving ticker plus the graph's own `count` + `map` + N joins.
macro_rules! cycles_group {
    ($crit:expr, $module:ident, $nodes:expr) => {{
        let run_for = RunFor::Cycles(CYCLES);
        let mut grp = $crit.benchmark_group(concat!("cycles_", stringify!($module)));
        grp.sample_size(20);
        grp.throughput(Throughput::Elements(CYCLES as u64 * $nodes));

        grp.bench_function("interpreted", |b| {
            b.iter_batched(
                || {
                    let g = GraphBuilder::new();
                    let trig = g.ticker(STEP);
                    let out = $module::wire(&g, &trig);
                    (g.build(), out)
                },
                |(mut runner, out)| {
                    runner.run(HISTORICAL, run_for).unwrap();
                    black_box(runner.value(&out))
                },
                BatchSize::SmallInput,
            )
        });

        grp.bench_function("nested", |b| {
            b.iter_batched(
                || {
                    let g = GraphBuilder::new();
                    let trig = g.ticker(STEP);
                    let out = $module::nested(&g, &trig);
                    (g.build(), out)
                },
                |(mut runner, out)| {
                    runner.run(HISTORICAL, run_for).unwrap();
                    black_box(runner.value(&out))
                },
                BatchSize::SmallInput,
            )
        });

        grp.finish();
    }};
}

fn cycles(crit: &mut Criterion) {
    cycles_group!(crit, depth_1, 4);
    cycles_group!(crit, depth_2, 5);
    cycles_group!(crit, depth_3, 6);
    cycles_group!(crit, depth_4, 7);
    cycles_group!(crit, depth_5, 8);
    cycles_group!(crit, depth_6, 9);
    cycles_group!(crit, depth_7, 10);
    cycles_group!(crit, depth_8, 11);
    cycles_group!(crit, depth_9, 12);
    cycles_group!(crit, depth_10, 13);
}

criterion_group!(benches, latency, cycles);
criterion_main!(benches);
