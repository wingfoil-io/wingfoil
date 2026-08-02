//! Branch/recombine at depths 1–10 on the wingfoil-next engine — the
//! breadth-first arm of the BFS-vs-DFS comparison.
//!
//! Port of legacy `legacy/wingfoil/benches/bfs_vs_dfs/wingfoil.rs`. The workload is
//! unchanged: at depth N the graph has 2^N source→sink paths, and a
//! breadth-first scheduler visits each node exactly once per tick, so the
//! measured cost should stay flat as the depth grows (see
//! `benches/bfs_vs_dfs/README.md`).
//!
//! Deliberate deviations from the legacy source, all mechanical:
//!
//! - the builder closure takes `(&GraphBuilder, &Stream<()>)` and returns an
//!   [`Upstream`] instead of `Rc<dyn Node> -> Rc<dyn Node>`, because next wires
//!   nodes through a builder (see `wingfoil_next::bencher`);
//! - legacy's free function `add(&s, &s)` — a `bimap` with *both* upstreams
//!   active — is spelled `s.join(&s, |a, b| a + b)` here, next's two-active-input
//!   combine. Same node, same both-arms-active shape, same one node per level.
//!
//! Run with: `cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil`

use criterion::{Criterion, criterion_group, criterion_main};
use wingfoil_next::bencher::add_bench;
use wingfoil_next::prelude::*;

fn branch_recombine(_graph: &GraphBuilder, trig: &Stream<()>, depth: usize) -> Upstream {
    let mut source = trig.count().map(|_: &u64| 1_u128);
    for _ in 0..depth {
        source = source.join(&source, |a: &u128, b: &u128| a + b);
    }
    source.upstream()
}

fn bench(crit: &mut Criterion) {
    for depth in 1..=10 {
        add_bench(
            crit,
            &format!("depth_{depth}"),
            move |g: &GraphBuilder, trig: &Stream<()>| branch_recombine(g, trig, depth),
        );
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
