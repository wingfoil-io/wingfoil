//! Benchmark for iceoryx2 adapter - Burst operations
//!
//! Port of legacy `legacy/wingfoil/benches/iceoryx2.rs`, verbatim apart from the
//! import path. `Burst<T>` is the **same type** on both trees (next re-exports
//! legacy's `tinyvec::TinyVec<[T; 1]>` rather than reimplementing it), so the
//! two bars measure identical code and must not diverge; the target exists so
//! the invocation survives the cutover.
//!
//! Run with: cargo bench --manifest-path crates/wingfoil/Cargo.toml --features iceoryx2 -- iceoryx2

use criterion::{Criterion, criterion_group, criterion_main};

use wingfoil::Burst;

/// Benchmark Burst operations
fn burst_push(crit: &mut Criterion) {
    let mut group = crit.benchmark_group("iceoryx2_burst");

    group.bench_function("push_single", |b| {
        b.iter(|| {
            let mut burst: Burst<i32> = Burst::default();
            burst.push(std::hint::black_box(42));
            std::hint::black_box(burst)
        });
    });

    group.bench_function("push_10", |b| {
        b.iter(|| {
            let mut burst: Burst<i32> = Burst::default();
            for i in 0..10 {
                burst.push(i);
            }
            std::hint::black_box(burst)
        });
    });

    group.bench_function("iterate_10", |b| {
        let mut burst = Burst::default();
        for i in 0..10 {
            burst.push(i);
        }
        b.iter(|| {
            let mut sum = 0i32;
            for item in std::hint::black_box(&burst) {
                sum += item;
            }
            sum
        });
    });

    group.bench_function("clone_empty", |b| {
        let source: Burst<i32> = Burst::default();
        b.iter(|| std::hint::black_box(source.clone()));
    });

    group.bench_function("clone_10", |b| {
        let mut source = Burst::default();
        for i in 0..10 {
            source.push(i);
        }
        b.iter(|| std::hint::black_box(source.clone()));
    });

    group.finish();
}

criterion_group!(benches, burst_push);
criterion_main!(benches);
