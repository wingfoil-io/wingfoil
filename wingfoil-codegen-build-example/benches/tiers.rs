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
mod fanout_10x10_inline {
    include!(concat!(env!("OUT_DIR"), "/fanout_10x10_inline.rs"));
}
mod fanout_10x10_dispatch {
    include!(concat!(env!("OUT_DIR"), "/fanout_10x10_dispatch.rs"));
}
mod fanout_10x10_standalone {
    include!(concat!(env!("OUT_DIR"), "/fanout_10x10_standalone.rs"));
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

/// The NxN fan-out graph from `wingfoil/benches/graph.rs` (the interpreted
/// engine's `10x10` bench), measured across all four tiers. One `count`
/// source feeds `width` parallel `depth`-deep identity-`map` chains, merged
/// into one stream; every node fires every cycle. Throughput in node-cycles/
/// sec makes the per-node dispatch cost comparable across engines.
fn fanout(c: &mut Criterion) {
    const WIDTH: usize = 10;
    const DEPTH: usize = 10;
    // ticker + constant + sample + fold + (width * depth) maps + merge.
    let nodes = WIDTH * DEPTH + 5;
    const CYCLES: u32 = 10_000;
    let run_for = RunFor::Cycles(CYCLES);
    let mut g = c.benchmark_group("fanout_10x10");
    g.sample_size(20);
    g.throughput(Throughput::Elements(CYCLES as u64 * nodes as u64));
    g.bench_function("interpreted", |b| {
        b.iter(|| {
            let (roots, values) = wiring::wire_fanout(WIDTH, DEPTH);
            Graph::new(roots, HISTORICAL, run_for).run().unwrap();
            black_box(values.peek_value())
        })
    });
    g.bench_function("dispatch", |b| {
        b.iter(|| {
            let (roots, values) = wiring::wire_fanout(WIDTH, DEPTH);
            fanout_10x10_dispatch::run(roots, HISTORICAL, run_for).unwrap();
            black_box(values.peek_value())
        })
    });
    g.bench_function("inline", |b| {
        b.iter(|| {
            let (roots, values) = wiring::wire_fanout(WIDTH, DEPTH);
            fanout_10x10_inline::run(roots, HISTORICAL, run_for).unwrap();
            black_box(values.peek_value())
        })
    });
    g.bench_function("standalone", |b| {
        b.iter(|| {
            #[rustfmt::skip]
            let inputs = fanout_10x10_standalone::Inputs {
                tick_0: NanoTime::new(100),
                constant_1: 1u64,
                fold_3: |acc: &mut u64, v: u64| *acc += v,
                map_4: black_box,
                map_5: black_box,
                map_6: black_box,
                map_7: black_box,
                map_8: black_box,
                map_9: black_box,
                map_10: black_box,
                map_11: black_box,
                map_12: black_box,
                map_13: black_box,
                map_14: black_box,
                map_15: black_box,
                map_16: black_box,
                map_17: black_box,
                map_18: black_box,
                map_19: black_box,
                map_20: black_box,
                map_21: black_box,
                map_22: black_box,
                map_23: black_box,
                map_24: black_box,
                map_25: black_box,
                map_26: black_box,
                map_27: black_box,
                map_28: black_box,
                map_29: black_box,
                map_30: black_box,
                map_31: black_box,
                map_32: black_box,
                map_33: black_box,
                map_34: black_box,
                map_35: black_box,
                map_36: black_box,
                map_37: black_box,
                map_38: black_box,
                map_39: black_box,
                map_40: black_box,
                map_41: black_box,
                map_42: black_box,
                map_43: black_box,
                map_44: black_box,
                map_45: black_box,
                map_46: black_box,
                map_47: black_box,
                map_48: black_box,
                map_49: black_box,
                map_50: black_box,
                map_51: black_box,
                map_52: black_box,
                map_53: black_box,
                map_54: black_box,
                map_55: black_box,
                map_56: black_box,
                map_57: black_box,
                map_58: black_box,
                map_59: black_box,
                map_60: black_box,
                map_61: black_box,
                map_62: black_box,
                map_63: black_box,
                map_64: black_box,
                map_65: black_box,
                map_66: black_box,
                map_67: black_box,
                map_68: black_box,
                map_69: black_box,
                map_70: black_box,
                map_71: black_box,
                map_72: black_box,
                map_73: black_box,
                map_74: black_box,
                map_75: black_box,
                map_76: black_box,
                map_77: black_box,
                map_78: black_box,
                map_79: black_box,
                map_80: black_box,
                map_81: black_box,
                map_82: black_box,
                map_83: black_box,
                map_84: black_box,
                map_85: black_box,
                map_86: black_box,
                map_87: black_box,
                map_88: black_box,
                map_89: black_box,
                map_90: black_box,
                map_91: black_box,
                map_92: black_box,
                map_93: black_box,
                map_94: black_box,
                map_95: black_box,
                map_96: black_box,
                map_97: black_box,
                map_98: black_box,
                map_99: black_box,
                map_100: black_box,
                map_101: black_box,
                map_102: black_box,
                map_103: black_box,
            };
            black_box(fanout_10x10_standalone::run(inputs, HISTORICAL, run_for))
        })
    });
    g.finish();
}

criterion_group!(benches, odds_evens, fanout, chain_dense, chain_sparse);
criterion_main!(benches);
