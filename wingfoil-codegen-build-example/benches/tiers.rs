//! Four-way engine comparison: interpreted `Graph::run` vs the generated
//! dispatch / inline / standalone tiers, over the graphs in
//! `bench_support/wiring.rs`. The generated runners are produced by
//! `build.rs` into `$OUT_DIR` at build time.
//!
//! Reported throughput is engine cycles per second; every measured iteration
//! includes wiring/setup (identical across tiers, amortized by the cycle
//! counts).

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil::*;

#[path = "../bench_support/wiring.rs"]
mod wiring;

mod oe_inline {
    include!(concat!(env!("OUT_DIR"), "/oe_inline.rs"));
}
mod oe_dispatch {
    include!(concat!(env!("OUT_DIR"), "/oe_dispatch.rs"));
}
mod oe_standalone {
    include!(concat!(env!("OUT_DIR"), "/oe_standalone.rs"));
}
mod chain_inline_16 {
    include!(concat!(env!("OUT_DIR"), "/chain_inline_16.rs"));
}
mod chain_dispatch_16 {
    include!(concat!(env!("OUT_DIR"), "/chain_dispatch_16.rs"));
}
mod chain_standalone_16 {
    include!(concat!(env!("OUT_DIR"), "/chain_standalone_16.rs"));
}
mod chain_inline_128 {
    include!(concat!(env!("OUT_DIR"), "/chain_inline_128.rs"));
}
mod chain_dispatch_128 {
    include!(concat!(env!("OUT_DIR"), "/chain_dispatch_128.rs"));
}
mod chain_standalone_128 {
    include!(concat!(env!("OUT_DIR"), "/chain_standalone_128.rs"));
}
mod chain_inline_1024 {
    include!(concat!(env!("OUT_DIR"), "/chain_inline_1024.rs"));
}
mod chain_dispatch_1024 {
    include!(concat!(env!("OUT_DIR"), "/chain_dispatch_1024.rs"));
}
mod chain_standalone_1024 {
    include!(concat!(env!("OUT_DIR"), "/chain_standalone_1024.rs"));
}
mod sparse_inline_128 {
    include!(concat!(env!("OUT_DIR"), "/sparse_inline_128.rs"));
}
mod sparse_dispatch_128 {
    include!(concat!(env!("OUT_DIR"), "/sparse_dispatch_128.rs"));
}
mod sparse_inline_1024 {
    include!(concat!(env!("OUT_DIR"), "/sparse_inline_1024.rs"));
}
mod sparse_dispatch_1024 {
    include!(concat!(env!("OUT_DIR"), "/sparse_dispatch_1024.rs"));
}

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

fn oe_inputs() -> oe_standalone::Inputs<
    impl FnMut(&mut u64, u64),
    impl FnMut(u64) -> bool,
    impl FnMut(bool) -> bool,
    impl FnMut(u64) -> String,
    impl FnMut(u64) -> String,
    impl FnMut(&mut Vec<String>, String),
> {
    oe_standalone::Inputs {
        tick_0: NanoTime::from(std::time::Duration::from_millis(10)),
        constant_1: 1,
        fold_3: |acc: &mut u64, v: u64| *acc += v,
        map_4: |i: u64| i.is_multiple_of(2),
        map_5: |b: bool| !b,
        map_7: |i: u64| format!("{i} is odd"),
        map_9: |i: u64| format!("{i} is even"),
        fold_11: |acc: &mut Vec<String>, v: String| acc.push(v),
    }
}

fn odds_evens(c: &mut Criterion) {
    const CYCLES: u32 = 5_000;
    let run_for = RunFor::Cycles(CYCLES);
    let mut g = c.benchmark_group("odds_evens");
    g.throughput(Throughput::Elements(CYCLES as u64));
    g.bench_function("interpreted", |b| {
        b.iter(|| {
            let (roots, values) = wiring::wire_odds_evens();
            Graph::new(roots, HISTORICAL, run_for).run().unwrap();
            black_box(values.peek_value().len())
        })
    });
    g.bench_function("dispatch", |b| {
        b.iter(|| {
            let (roots, values) = wiring::wire_odds_evens();
            oe_dispatch::run(roots, HISTORICAL, run_for).unwrap();
            black_box(values.peek_value().len())
        })
    });
    g.bench_function("inline", |b| {
        b.iter(|| {
            let (roots, values) = wiring::wire_odds_evens();
            oe_inline::run(roots, HISTORICAL, run_for).unwrap();
            black_box(values.peek_value().len())
        })
    });
    g.bench_function("standalone", |b| {
        b.iter(|| {
            let (out,) = oe_standalone::run(oe_inputs(), HISTORICAL, run_for);
            black_box(out.len())
        })
    });
    g.finish();
}

/// One dense-chain benchmark set: every node fires every cycle, so
/// throughput in node-cycles/sec measures per-node dispatch overhead.
macro_rules! chain_group {
    ($c:expr, $len:expr, $inline:ident, $dispatch:ident, $standalone:ident) => {{
        let len: usize = $len;
        let nodes = len + 2;
        // Fixed node-cycle budget so every size runs a comparable amount of
        // work per iteration.
        let cycles = ((1_000_000 / nodes).max(500)) as u32;
        let run_for = RunFor::Cycles(cycles);
        let mut g = $c.benchmark_group(format!("chain_dense_{len}"));
        g.sample_size(10);
        g.throughput(Throughput::Elements(cycles as u64 * nodes as u64));
        g.bench_function("interpreted", |b| {
            b.iter(|| {
                let (roots, values) = wiring::wire_chain(len);
                Graph::new(roots, HISTORICAL, run_for).run().unwrap();
                black_box(values.peek_value())
            })
        });
        g.bench_function("dispatch", |b| {
            b.iter(|| {
                let (roots, values) = wiring::wire_chain(len);
                $dispatch::run(roots, HISTORICAL, run_for).unwrap();
                black_box(values.peek_value())
            })
        });
        g.bench_function("inline", |b| {
            b.iter(|| {
                let (roots, values) = wiring::wire_chain(len);
                $inline::run(roots, HISTORICAL, run_for).unwrap();
                black_box(values.peek_value())
            })
        });
        g.bench_function("standalone", |b| {
            b.iter(|| {
                let inputs = $standalone::Inputs {
                    tick_0: NanoTime::new(100),
                    constant_1: 1u64,
                };
                black_box($standalone::run(inputs, HISTORICAL, run_for))
            })
        });
        g.finish();
    }};
}

fn chain_dense(c: &mut Criterion) {
    chain_group!(
        c,
        16,
        chain_inline_16,
        chain_dispatch_16,
        chain_standalone_16
    );
    chain_group!(
        c,
        128,
        chain_inline_128,
        chain_dispatch_128,
        chain_standalone_128
    );
    chain_group!(
        c,
        1024,
        chain_inline_1024,
        chain_dispatch_1024,
        chain_standalone_1024
    );
}

/// One sparse benchmark set: ~999 of 1000 cycles tick only the fast counter,
/// leaving the chain quiet. Throughput in cycles/sec measures the quiet-node
/// floor of each engine.
macro_rules! sparse_group {
    ($c:expr, $len:expr, $inline:ident, $dispatch:ident) => {{
        let len: usize = $len;
        const CYCLES: u32 = 20_000;
        let run_for = RunFor::Cycles(CYCLES);
        let mut g = $c.benchmark_group(format!("chain_sparse_{len}"));
        g.sample_size(10);
        g.throughput(Throughput::Elements(CYCLES as u64));
        g.bench_function("interpreted", |b| {
            b.iter(|| {
                let roots = wiring::wire_sparse(len);
                Graph::new(roots, HISTORICAL, run_for).run().unwrap()
            })
        });
        g.bench_function("dispatch", |b| {
            b.iter(|| {
                let roots = wiring::wire_sparse(len);
                $dispatch::run(roots, HISTORICAL, run_for).unwrap()
            })
        });
        g.bench_function("inline", |b| {
            b.iter(|| {
                let roots = wiring::wire_sparse(len);
                $inline::run(roots, HISTORICAL, run_for).unwrap()
            })
        });
        g.finish();
    }};
}

fn chain_sparse(c: &mut Criterion) {
    sparse_group!(c, 128, sparse_inline_128, sparse_dispatch_128);
    sparse_group!(c, 1024, sparse_inline_1024, sparse_dispatch_1024);
}

criterion_group!(benches, odds_evens, chain_dense, chain_sparse);
criterion_main!(benches);
