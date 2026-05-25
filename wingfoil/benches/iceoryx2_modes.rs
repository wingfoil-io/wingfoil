//! Benchmarks for iceoryx2 adapter modes (Spin vs Threaded vs Signaled)
//!
//! Run with: cargo bench --features iceoryx2-beta -- iceoryx2_modes

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iceoryx2::prelude::ZeroCopySend;
use wingfoil::adapters::iceoryx2::{
    Iceoryx2Mode, Iceoryx2ServiceVariant, Iceoryx2SubOpts, iceoryx2_pub_with, iceoryx2_sub_opts,
};
use wingfoil::{Burst, Graph, NodeOperators, RunFor, RunMode, StreamOperators, ticker};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct BenchData {
    value: u64,
    timestamp: u64,
}

fn bench_mode(c: &mut Criterion, mode: Iceoryx2Mode, mode_name: &str) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let service_name = format!("wingfoil/bench/{}/{}", mode_name, n);

    c.bench_function(&format!("iceoryx2_{}", mode_name), |b| {
        b.iter(|| {
            let opts = Iceoryx2SubOpts {
                variant: Iceoryx2ServiceVariant::Local,
                mode,
                ..Default::default()
            };
            let sub = iceoryx2_sub_opts::<BenchData>(&service_name, opts);
            let collected = sub.collapse().collect();

            let upstream = ticker(Duration::from_micros(100)).produce(|| {
                let mut b = Burst::default();
                b.push(BenchData {
                    value: 42,
                    timestamp: 0,
                });
                b
            });
            let pub_node =
                iceoryx2_pub_with(upstream, &service_name, Iceoryx2ServiceVariant::Local);

            let mut graph = Graph::new(
                vec![pub_node, collected.clone().as_node()],
                RunMode::RealTime,
                RunFor::Cycles(100),
            );
            graph.run().unwrap();
        })
    });
}

fn iceoryx2_modes_benchmark(c: &mut Criterion) {
    bench_mode(c, Iceoryx2Mode::Spin, "spin");
    bench_mode(c, Iceoryx2Mode::Threaded, "threaded");
    bench_mode(c, Iceoryx2Mode::Signaled, "signaled");
}

criterion_group!(benches, iceoryx2_modes_benchmark);
criterion_main!(benches);
