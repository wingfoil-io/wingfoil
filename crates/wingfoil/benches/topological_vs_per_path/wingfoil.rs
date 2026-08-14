//! Branch/recombine at depths 1–10 on the wingfoil engine — the
//! topological-sort arm of the comparison against per-path propagation.
//!
//! Port of legacy `legacy/wingfoil/benches/bfs_vs_dfs/wingfoil.rs`. The workload is
//! unchanged: at depth N the graph has 2^N source→sink paths, and a
//! topologically sorted scheduler visits each node exactly once per tick, so
//! the measured cost should stay flat as the depth grows (see
//! `benches/topological_vs_per_path/README.md`).
//!
//! Each depth is wired **once**, in a `nitro!` block, and measured on all three
//! engines the macro emits from those same tokens:
//!
//! - `interpreted()` — the dyn-dispatched graph (the tier legacy measures, and
//!   the one the cross-library comparison is about);
//! - `compiled()` — the whole graph emitted as a single monomorphized function
//!   owning its own run loop;
//! - `nested()` — the same DAG mounted as a single compiled island: a
//!   monomorphized straight-line interior, one dyn call at the boundary per
//!   activation.
//!
//! # The graphs are self-contained, and run for a fixed cycle count
//!
//! The driving ticker lives *inside* the `nitro!` block, and each group runs
//! `RunFor::Cycles(CYCLES)` per sample rather than one tick. Whole-run time
//! divided by `CYCLES` is the graph's own per-tick cost. Both properties are
//! deliberate, and they buy two different things:
//!
//! - **Nothing but the graph is under the measurement.** This target used to
//!   carry a second sweep driven through `wingfoil::bencher::add_bench`, which
//!   runs the graph on a worker thread and hands off one tick per criterion
//!   sample through a spinning `AtomicU8` — two `SeqCst` round-trips and a
//!   cross-core cache-line transfer. That handshake costs a few hundred
//!   nanoseconds; these graphs are 4–13 nodes and cost tens. The samples were
//!   therefore mostly harness — flat long before the graph was, and
//!   non-monotonic in depth (the island read 415 ns at depth 1 and 300 ns at
//!   depth 2, which no amount of added nodes can explain). Driving from an
//!   internal ticker removes the handshake outright rather than subtracting an
//!   estimate of it; `../graph.rs`'s `node` bench still measures that floor in
//!   isolation for anyone who wants the number.
//! - **It is the condition `compiled()` requires.** `nitro!` emits a
//!   whole-program `compiled()` only for a graph with no stream parameters; an
//!   input-taking graph is a component by definition and gets `wire` + `nested`
//!   only. A graph fed by the bencher *must* take an input, so that tier could
//!   never appear under the old harness at all.
//!
//! The cost of the change is comparability: [`reactive.rs`](reactive.rs) and
//! [`async_streams.rs`](async_streams.rs) call straight into their library on
//! the criterion thread and time one source event per sample. They never paid a
//! handshake, so dropping wingfoil's brings the two closer — but a per-cycle
//! figure and a per-event figure are not the same measurement. Read the
//! *slopes*, which are what the O(N)-against-O(2^N) claim rests on, and treat
//! the ratios as indicative. The README says so wherever it quotes one.
//!
//! The groups stay named `cycles_depth_N` even though there is no longer a
//! per-tick sweep to distinguish them from. All three targets in this directory
//! write into the same `target/criterion/` tree, and the other two name their
//! benchmarks `depth_1`..`depth_10`; a rename here would collide with them and
//! whichever ran last would own the directory.
//!
//! # Wiring
//!
//! [`branch_recombine!`] generates each depth's `nitro!` block from a list of
//! binding names — `branch_recombine!(src_depth_3, s0 s1 s2 s3)` is the source
//! binding followed by one per join level. The expansion is a flat run of
//! `let`s, which is the point: `nitro!` requires straight-line wiring, since
//! nodes built inside a `for` loop are invisible to the compiled emission. A
//! `macro_rules!` muncher expands *before* the proc macro sees the body, so it
//! gets to write that straight line for us. Every varying identifier is passed
//! in from the call site rather than invented inside the macro, because
//! `macro_rules!` local-variable hygiene gives each recursion step its own
//! context and macro-invented bindings would not resolve across them.
//!
//! Deliberate deviations from the legacy source, all mechanical:
//!
//! - the wiring closure takes `&GraphBuilder` and returns a [`Stream`] instead
//!   of `Rc<dyn Node> -> Rc<dyn Node>`, because wingfoil wires nodes through a
//!   builder;
//! - legacy's free function `add(&s, &s)` — a `bimap` with *both* upstreams
//!   active — is spelled `s.join(&s, |a, b| a + b)` here, wingfoil's two-active-input
//!   combine. Same node, same both-arms-active shape, same one node per level;
//! - every level's sum passes through [`std::hint::black_box`], and so does the
//!   source value, which legacy does not need. Without the fences the compiled
//!   tiers are free to notice that summing one value 2^N times is a shift, and
//!   to collapse the whole chain — turning those bars into a measurement of
//!   nothing. They cost the interpreted tier nothing measurable (dispatch
//!   dominates there), so every tier stays honest; `benches/tiers.rs` fences its
//!   map closures the same way;
//! - the source is legacy's `count()` mapped to a `u128`, but it forwards the
//!   count rather than legacy's constant `1`, so the value entering the stack
//!   changes every tick and nothing downstream can be hoisted out of the run
//!   loop. One map node either way;
//! - legacy drives one tick per sample through its own `bencher`; this target
//!   drives `CYCLES` ticks from an internal ticker, for the reasons above.
//!
//! Run with: `cargo bench -p wingfoil --bench bfs_vs_dfs_wingfoil`

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);
const CYCLES: u32 = 10_000;

/// One `nitro!` block per depth: a ticker-driven source, then one `join` level
/// per name after the first.
///
/// Each `join` reads the *same* upstream twice, so at depth N the graph has 2^N
/// source→sink paths across N + 3 nodes (ticker, `count`, `map`, N joins).
/// Invoked as `branch_recombine!(src_depth_2, s0 s1 s2)`: `s0` names the source
/// binding, every name after it one level.
///
/// The `@build` rules are a token muncher — they walk the level list, threading
/// the previous level's binding through as `$prev` and accumulating finished
/// `let` statements in `$acc` — and the terminating rule emits them into the
/// `nitro!` block in one flat run. It has to be one flat run: `nitro!` reads the
/// body as straight-line wiring statements and rejects both loops and nested
/// blocks. Note that `g` appears only in that final rule, never in the
/// accumulator, so it is created in a single expansion; and that `$src` is
/// threaded all the way through unused rather than re-invented at the end, for
/// the same hygiene reason.
macro_rules! branch_recombine {
    ($name:ident, $src:ident $($lvl:ident)*) => {
        branch_recombine!(@build $name, $src, $src, [$($lvl)*], []);
    };
    (@build $name:ident, $src:ident, $prev:ident, [$head:ident $($tail:ident)*], [$($acc:tt)*]) => {
        branch_recombine!(@build $name, $src, $head, [$($tail)*], [$($acc)*
            let $head = $prev.join(&$prev, |a: &u128, b: &u128| black_box(a + b));
        ]);
    };
    (@build $name:ident, $src:ident, $last:ident, [], [$($acc:tt)*]) => {
        wingfoil::nitro! {
            fn $name(g: &GraphBuilder) -> Stream<u128> {
                let $src = g.ticker(STEP).count().map(|c: &u64| black_box(*c as u128));
                $($acc)*
                $last
            }
        }
    };
}

branch_recombine!(src_depth_1, s0 s1);
branch_recombine!(src_depth_2, s0 s1 s2);
branch_recombine!(src_depth_3, s0 s1 s2 s3);
branch_recombine!(src_depth_4, s0 s1 s2 s3 s4);
branch_recombine!(src_depth_5, s0 s1 s2 s3 s4 s5);
branch_recombine!(src_depth_6, s0 s1 s2 s3 s4 s5 s6);
branch_recombine!(src_depth_7, s0 s1 s2 s3 s4 s5 s6 s7);
branch_recombine!(src_depth_8, s0 s1 s2 s3 s4 s5 s6 s7 s8);
branch_recombine!(src_depth_9, s0 s1 s2 s3 s4 s5 s6 s7 s8 s9);
branch_recombine!(src_depth_10, s0 s1 s2 s3 s4 s5 s6 s7 s8 s9 s10);

/// One fixed-cycle group per depth: `interpreted`, `compiled`, `nested`.
///
/// `$depth` is the number of join levels — node count is that plus the driving
/// ticker, `count` and `map`. Construction is hoisted into `iter_batched`'s
/// setup so only dispatch is timed, as in `../tiers.rs`. Read the three bars
/// against each other within a group, and the groups against each other as a
/// slope in depth: this is where "each node once per tick" turns into a number
/// — an added level is one more node, not twice the work.
macro_rules! cycles_group {
    ($crit:expr, $name:literal, $module:ident, $depth:expr) => {{
        let run_for = RunFor::Cycles(CYCLES);
        let nodes: u64 = $depth + 3;
        let mut grp = $crit.benchmark_group($name);
        grp.sample_size(20);
        grp.throughput(Throughput::Elements(CYCLES as u64 * nodes));

        grp.bench_function("interpreted", |b| {
            b.iter_batched(
                || $module::interpreted(),
                |(mut runner, out)| {
                    runner.run(HISTORICAL, run_for).unwrap();
                    black_box(runner.value(out))
                },
                BatchSize::PerIteration,
            )
        });

        // No setup to hoist: `compiled()` emits its wiring as straight-line
        // locals, so build and run are one generated function by construction.
        grp.bench_function("compiled", |b| {
            b.iter(|| black_box($module::compiled(HISTORICAL, run_for).unwrap()))
        });

        grp.bench_function("nested", |b| {
            b.iter_batched(
                || {
                    let g = GraphBuilder::new();
                    let out = $module::nested(&g);
                    (g.build(), out)
                },
                |(mut runner, out)| {
                    runner.run(HISTORICAL, run_for).unwrap();
                    black_box(runner.value(&out))
                },
                BatchSize::PerIteration,
            )
        });

        grp.finish();
    }};
}

fn cycles(crit: &mut Criterion) {
    cycles_group!(crit, "cycles_depth_1", src_depth_1, 1);
    cycles_group!(crit, "cycles_depth_2", src_depth_2, 2);
    cycles_group!(crit, "cycles_depth_3", src_depth_3, 3);
    cycles_group!(crit, "cycles_depth_4", src_depth_4, 4);
    cycles_group!(crit, "cycles_depth_5", src_depth_5, 5);
    cycles_group!(crit, "cycles_depth_6", src_depth_6, 6);
    cycles_group!(crit, "cycles_depth_7", src_depth_7, 7);
    cycles_group!(crit, "cycles_depth_8", src_depth_8, 8);
    cycles_group!(crit, "cycles_depth_9", src_depth_9, 9);
    cycles_group!(crit, "cycles_depth_10", src_depth_10, 10);
}

criterion_group!(benches, cycles);
criterion_main!(benches);
