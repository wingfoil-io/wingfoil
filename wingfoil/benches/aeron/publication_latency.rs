//! Publication latency benchmarks for Aeron transport.
//!
//! Measures the latency of publishing messages using the `offer` method
//! across different message sizes.

#[path = "common/mod.rs"]
mod common;

use common::MessageSize;
use common::rusteron_support::BenchContext;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;
use wingfoil::adapters::aeron::AeronPublisherBackend;
use wingfoil::adapters::aeron::rusteron_backend::RusteronPublisher;

/// Benchmark offer latency across message sizes.
fn bench_offer(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let publication = ctx.add_publication(2001);
    let mut publisher = RusteronPublisher::new(publication);

    let mut group = c.benchmark_group("offer");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let buffer = size.create_buffer();
        group.throughput(Throughput::Bytes(size.bytes() as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(size.name()),
            &buffer,
            |b, buf| {
                b.iter(|| {
                    let _ = black_box(publisher.offer(black_box(buf)));
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_offer);
criterion_main!(benches);
