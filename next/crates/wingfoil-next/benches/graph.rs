//! Graph overhead: the cost of one engine cycle through a trivial DAG.
//!
//! Port of classic `wingfoil/benches/graph.rs`. The workload is unchanged — a
//! trigger-driven `count` source fanned into `width` chains of `depth` identity
//! maps, recombined by one n-ary merge, with every node ticking on every engine
//! cycle — so the numbers stay directly comparable against the classic bar.
//!
//! Deliberate deviations from the classic source, both mechanical:
//!
//! - the builder closure takes `(&GraphBuilder, &Stream<()>)` and returns an
//!   [`Upstream`] instead of `Rc<dyn Node> -> Rc<dyn Node>`, because next wires
//!   nodes through a builder (see `wingfoil_next::bencher`);
//! - `map` takes `&T` rather than `T`, so classic's `map(std::hint::black_box)`
//!   is spelled `map(|i: &u64| std::hint::black_box(*i))`. Same one black-boxed
//!   copy per node per cycle.
//!
//! The node count matches classic exactly: `width * depth` maps plus the
//! `count` source plus one merge (next's `merge_all` wires a single `MergeN`
//! node, as classic's `merge(vec)` does).
//!
//! Run with: `cargo bench -p wingfoil-next --features bench --bench graph`

use criterion::{Criterion, criterion_group, criterion_main};
use wingfoil_next::bencher::add_bench;
use wingfoil_next::prelude::*;

fn node(_graph: &GraphBuilder, trig: &Stream<()>) -> Upstream {
    trig.upstream()
}

fn nodes(_graph: &GraphBuilder, trig: &Stream<()>, width: usize, depth: usize) -> Upstream {
    let src = trig.count();
    let streams = (0..width)
        .map(|_| {
            let mut stream = src.clone();
            for _ in 0..depth {
                stream = stream.map(|i: &u64| std::hint::black_box(*i));
            }
            stream
        })
        .collect::<Vec<_>>();
    let (first, rest) = streams
        .split_first()
        .expect("invariant: bench width is always >= 1");
    let rest: Vec<&Stream<u64>> = rest.iter().collect();
    first.merge_all(&rest).upstream()
}

fn bench(crit: &mut Criterion) {
    add_bench(crit, "node", node);
    add_bench(crit, "10x10", |g: &GraphBuilder, trig: &Stream<()>| {
        nodes(g, trig, 10, 10)
    });
    add_bench(crit, "100x100", |g: &GraphBuilder, trig: &Stream<()>| {
        nodes(g, trig, 100, 100)
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
