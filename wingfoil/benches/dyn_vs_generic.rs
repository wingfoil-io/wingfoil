//! `dyn` (dynamic dispatch) vs generics (static / monomorphised dispatch).
//!
//! Wingfoil is built on `Rc<dyn Node>` and boxed closures (see `MapStream`).
//! A recurring objection is "dynamic dispatch is slow". This benchmark exists
//! to measure *where that is true and where it is not*, so the trade-off can be
//! reasoned about with numbers instead of folklore.
//!
//! The headline finding: the cost of `dyn` is **not** the vtable indirection
//! (a well-predicted indirect call is ~1ns). The cost is that a virtual call is
//! an **optimisation barrier** — the compiler cannot inline through it, and
//! everything inlining unlocks (constant folding, auto-vectorisation, dead-code
//! elimination) is lost. So the gap is enormous when the per-call work is tiny
//! and vectorisable, and vanishes the moment each call does real work.
//!
//! Run with:  `cargo bench --bench dyn_vs_generic`
//!
//! Each group compares the *same source operation* under static vs dynamic
//! dispatch — only the dispatch mechanism differs.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

const N: usize = 8192;

fn data() -> Vec<u64> {
    (0..N as u64)
        .map(|x| x.wrapping_mul(2_654_435_761))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// The operations under test.
//
//  `tiny`  — a couple of arithmetic ops. Cheaper than a function call.
//  `heavy` — a fixed-iteration integer mixing loop (~tens of ns). Realistic
//            "this node actually computes something" work.
// ─────────────────────────────────────────────────────────────────────────────

#[inline(always)]
fn tiny(x: u64) -> u64 {
    x.wrapping_mul(2).wrapping_add(1)
}

#[inline(always)]
fn heavy(x: u64) -> u64 {
    // A small FNV-style mixing loop. Bounded, branch-free, but enough real
    // work that a ~1ns indirect call is noise against it.
    let mut h = x;
    for _ in 0..24 {
        h ^= h >> 15;
        h = h.wrapping_mul(0x2545_F491_4F6C_DD1D);
        h = h.rotate_left(27);
    }
    h
}

// Static: the operation is a generic type parameter, so the whole loop is
// monomorphised and the body is inlined into the caller.
#[inline(never)]
fn apply_static<F: Fn(u64) -> u64>(data: &[u64], f: F) -> u64 {
    data.iter().map(|&x| f(x)).fold(0u64, u64::wrapping_add)
}

// Dynamic: the operation is a trait object. Every element pays an indirect
// call the optimiser cannot see through.
#[inline(never)]
fn apply_dyn(data: &[u64], f: &dyn Fn(u64) -> u64) -> u64 {
    data.iter().map(|&x| f(x)).fold(0u64, u64::wrapping_add)
}

fn bench_tiny(c: &mut Criterion) {
    let data = data();
    let mut g = c.benchmark_group("tiny_op");
    g.bench_function("static", |b| {
        b.iter(|| apply_static(black_box(&data), tiny))
    });
    g.bench_function("dyn", |b| {
        let f: &dyn Fn(u64) -> u64 = &tiny;
        b.iter(|| apply_dyn(black_box(&data), f))
    });
    g.finish();
}

fn bench_heavy(c: &mut Criterion) {
    let data = data();
    let mut g = c.benchmark_group("heavy_op");
    g.bench_function("static", |b| {
        b.iter(|| apply_static(black_box(&data), heavy))
    });
    g.bench_function("dyn", |b| {
        let f: &dyn Fn(u64) -> u64 = &heavy;
        b.iter(|| apply_dyn(black_box(&data), f))
    });
    g.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// A staged pipeline — the shape a stream framework actually has.
//
// Static: a fixed composition of combinators, fully inlined into one loop body.
// Dynamic: `Vec<Box<dyn Fn>>`, the stages applied in a loop — exactly what a
//          runtime-wired graph must do, because the stage list is data.
// ─────────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn pipeline_static(data: &[u64]) -> u64 {
    data.iter()
        .map(|&x| {
            let x = x.wrapping_add(1);
            let x = x.wrapping_mul(3);
            let x = x ^ (x >> 7);
            let x = x.wrapping_add(0x9E37_79B9);
            let x = x.rotate_left(11);
            x.wrapping_mul(2)
        })
        .fold(0u64, u64::wrapping_add)
}

#[inline(never)]
fn pipeline_dyn(data: &[u64], stages: &[Box<dyn Fn(u64) -> u64>]) -> u64 {
    data.iter()
        .map(|&x| {
            let mut v = x;
            for s in stages {
                v = s(v);
            }
            v
        })
        .fold(0u64, u64::wrapping_add)
}

fn bench_pipeline(c: &mut Criterion) {
    let data = data();
    let stages: Vec<Box<dyn Fn(u64) -> u64>> = vec![
        Box::new(|x: u64| x.wrapping_add(1)),
        Box::new(|x: u64| x.wrapping_mul(3)),
        Box::new(|x: u64| x ^ (x >> 7)),
        Box::new(|x: u64| x.wrapping_add(0x9E37_79B9)),
        Box::new(|x: u64| x.rotate_left(11)),
        Box::new(|x: u64| x.wrapping_mul(2)),
    ];
    let mut g = c.benchmark_group("pipeline_6_stages");
    g.bench_function("static", |b| b.iter(|| pipeline_static(black_box(&data))));
    g.bench_function("dyn", |b| {
        b.iter(|| pipeline_dyn(black_box(&data), black_box(&stages)))
    });
    g.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Branch prediction: the hidden variable in "how slow is dyn?".
//
// An indirect call whose target is the same every time is predicted perfectly
// by the CPU; the vtable barely costs anything. An indirect call whose target
// keeps changing stalls the pipeline on a mispredict. Same trait, same work,
// only the *homogeneity of the call site* differs.
// ─────────────────────────────────────────────────────────────────────────────

trait Op {
    fn apply(&self, x: u64) -> u64;
}
struct AddOne;
struct MulThree;
struct Xor;
struct Rotate;
impl Op for AddOne {
    fn apply(&self, x: u64) -> u64 {
        x.wrapping_add(1)
    }
}
impl Op for MulThree {
    fn apply(&self, x: u64) -> u64 {
        x.wrapping_mul(3)
    }
}
impl Op for Xor {
    fn apply(&self, x: u64) -> u64 {
        x ^ (x >> 7)
    }
}
impl Op for Rotate {
    fn apply(&self, x: u64) -> u64 {
        x.rotate_left(11)
    }
}

#[inline(never)]
fn run_ops(ops: &[Box<dyn Op>], seed: u64) -> u64 {
    let mut acc = seed;
    for op in ops {
        acc = op.apply(acc);
    }
    acc
}

fn bench_prediction(c: &mut Criterion) {
    let len = N;
    // Same call site, same target every iteration → predicted.
    let homogeneous: Vec<Box<dyn Op>> = (0..len).map(|_| Box::new(AddOne) as Box<dyn Op>).collect();
    // Target rotates every iteration → the predictor cannot keep up.
    let heterogeneous: Vec<Box<dyn Op>> = (0..len)
        .map(|i| match i % 4 {
            0 => Box::new(AddOne) as Box<dyn Op>,
            1 => Box::new(MulThree) as Box<dyn Op>,
            2 => Box::new(Xor) as Box<dyn Op>,
            _ => Box::new(Rotate) as Box<dyn Op>,
        })
        .collect();

    let mut g = c.benchmark_group("dyn_dispatch_prediction");
    g.bench_function("homogeneous_targets", |b| {
        b.iter(|| run_ops(black_box(&homogeneous), black_box(1)))
    });
    g.bench_function("heterogeneous_targets", |b| {
        b.iter(|| run_ops(black_box(&heterogeneous), black_box(1)))
    });
    g.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// The Wingfoil shape: a collection of stateful nodes, each cycled per tick.
//
// This is the case that actually matters for a graph engine. Static: a
// homogeneous `Vec<Accum>` cycled in a monomorphic loop. Dynamic: `Vec<Box<dyn
// TickNode>>`, which is what a heterogeneous DAG *must* use — you cannot put a
// MapStream, a FilterStream and a FoldStream in the same `Vec<T>` without
// erasing the type. Each cycle does a realistic small amount of work.
// ─────────────────────────────────────────────────────────────────────────────

trait TickNode {
    fn cycle(&mut self, input: u64) -> u64;
}

#[derive(Default)]
struct Accum {
    state: u64,
}
impl TickNode for Accum {
    fn cycle(&mut self, input: u64) -> u64 {
        // A small amount of real per-node work: mix input into running state.
        self.state = self.state.wrapping_add(input);
        self.state ^= self.state >> 13;
        self.state = self.state.wrapping_mul(0x100_0000_01B3);
        self.state
    }
}

#[inline(never)]
fn cycle_static(nodes: &mut [Accum], input: u64) -> u64 {
    let mut acc = 0u64;
    for n in nodes.iter_mut() {
        acc = acc.wrapping_add(n.cycle(input));
    }
    acc
}

#[inline(never)]
fn cycle_dyn(nodes: &mut [Box<dyn TickNode>], input: u64) -> u64 {
    let mut acc = 0u64;
    for n in nodes.iter_mut() {
        acc = acc.wrapping_add(n.cycle(input));
    }
    acc
}

fn bench_nodes(c: &mut Criterion) {
    let width = 1024;
    let mut g = c.benchmark_group("stateful_nodes");
    g.bench_function("static_vec", |b| {
        b.iter_batched_ref(
            || (0..width).map(|_| Accum::default()).collect::<Vec<_>>(),
            |nodes| cycle_static(black_box(nodes), black_box(7)),
            BatchSize::SmallInput,
        )
    });
    g.bench_function("dyn_vec", |b| {
        b.iter_batched_ref(
            || {
                (0..width)
                    .map(|_| Box::new(Accum::default()) as Box<dyn TickNode>)
                    .collect::<Vec<_>>()
            },
            |nodes| cycle_dyn(black_box(nodes), black_box(7)),
            BatchSize::SmallInput,
        )
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_tiny,
    bench_heavy,
    bench_pipeline,
    bench_prediction,
    bench_nodes,
);
criterion_main!(benches);
