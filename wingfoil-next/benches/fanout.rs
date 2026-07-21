//! Benchmark the `graph!` macro path on the "10x10" fan-out graph (mirrors
//! the classic-engine `wingfoil/benches/graph.rs` shape): one `count` source
//! fanned out into 10 parallel 10-deep identity-`map` chains, merged into one
//! stream. Every node fires every cycle.
//!
//! Two `graph!`-derived engines are measured against each other:
//! - `interpreted()` — dynamic, shared-node engine;
//! - `compiled()` — the fully monomorphized straight-line runner.
//!
//! Throughput is reported in node-cycles/sec (engine-cycles x 105 nodes) so
//! the numbers line up with the classic codegen tiers in
//! `wingfoil-codegen-build-example/benches/tiers.rs`.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

include!("../bench_support/fanout_10x10.rs");

fn fanout(c: &mut Criterion) {
    // ticker + constant + sample + fold + 10*10 maps + merge.
    const NODES: u64 = 10 * 10 + 5;
    const CYCLES: u32 = 10_000;
    let run_for = RunFor::Cycles(CYCLES);

    let mut g = c.benchmark_group("fanout_10x10_next");
    g.sample_size(20);
    g.throughput(Throughput::Elements(CYCLES as u64 * NODES));

    g.bench_function("interpreted", |b| {
        b.iter(|| {
            let (mut runner, out) = fanout::interpreted();
            runner.run(HISTORICAL, run_for).unwrap();
            black_box(runner.value(out))
        })
    });

    g.bench_function("compiled", |b| {
        b.iter(|| black_box(fanout::compiled(HISTORICAL, run_for).unwrap()))
    });

    g.finish();
}

criterion_group!(benches, fanout);
criterion_main!(benches);
