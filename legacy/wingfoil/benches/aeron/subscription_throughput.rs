//! Subscription throughput benchmarks for Aeron transport.
//!
//! Measures the throughput of receiving messages across different scenarios:
//! - Basic polling
//! - Polling with message parsing
//! - Burst throughput

#[path = "common/mod.rs"]
mod common;

use common::MessageSize;
use common::rusteron_support::BenchContext;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::thread;
use std::time::Duration;
use wingfoil::adapters::aeron::rusteron_backend::{RusteronPublisher, RusteronSubscriber};
use wingfoil::adapters::aeron::{AeronPublisherBackend, AeronSubscriberBackend};

/// Run all subscription benchmarks with a single shared media driver.
fn bench_all(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    bench_poll(c, &ctx);
    bench_poll_with_parsing(c, &ctx);
    bench_burst_throughput(c, &ctx);
}

/// Benchmark basic poll throughput.
fn bench_poll(c: &mut Criterion, ctx: &BenchContext) {
    let mut group = c.benchmark_group("poll");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(10);

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let stream_id = 5001 + size.bytes() as i32;
        let (publication, subscription) = ctx.add_pub_sub(stream_id);

        let mut publisher = RusteronPublisher::new(publication);
        let mut subscriber = RusteronSubscriber::new(subscription);

        group.throughput(Throughput::Bytes(size.bytes() as u64));

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            let buffer = size.create_buffer();

            b.iter(|| {
                let _ = publisher.offer(&buffer);
                let count = subscriber.poll(&mut |_fragment: &[u8]| {}).unwrap_or(0);
                black_box(count)
            });
        });
    }

    group.finish();
}

/// Benchmark subscription with message parsing.
fn bench_poll_with_parsing(c: &mut Criterion, ctx: &BenchContext) {
    let mut group = c.benchmark_group("poll_parse");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let stream_id = 5101 + size.bytes() as i32;
        let (publication, subscription) = ctx.add_pub_sub(stream_id);

        let mut publisher = RusteronPublisher::new(publication);
        let mut subscriber = RusteronSubscriber::new(subscription);

        group.throughput(Throughput::Bytes(size.bytes() as u64));

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            let mut buffer = size.create_buffer();
            buffer[0..8].copy_from_slice(&42i64.to_le_bytes());

            b.iter(|| {
                let _ = publisher.offer(&buffer);

                let mut value: i64 = 0;
                let count = subscriber
                    .poll(&mut |fragment: &[u8]| {
                        if fragment.len() >= 8 {
                            let bytes: [u8; 8] = fragment[0..8].try_into().unwrap();
                            value = i64::from_le_bytes(bytes);
                        }
                    })
                    .unwrap_or(0);

                black_box((count, value))
            });
        });
    }

    group.finish();
}

/// Benchmark burst subscription throughput.
fn bench_burst_throughput(c: &mut Criterion, ctx: &BenchContext) {
    let mut group = c.benchmark_group("poll_burst");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);
    let burst_size = 100;

    for size in [MessageSize::Small, MessageSize::Medium] {
        let stream_id = 5201 + size.bytes() as i32;
        let (publication, subscription) = ctx.add_pub_sub(stream_id);

        let mut publisher = RusteronPublisher::new(publication);
        let mut subscriber = RusteronSubscriber::new(subscription);

        group.throughput(Throughput::Elements(burst_size as u64));

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            let buffer = size.create_buffer();

            b.iter(|| {
                for _ in 0..burst_size {
                    while publisher.offer(&buffer).is_err() {
                        thread::yield_now();
                    }
                }

                let mut total = 0;
                while total < burst_size {
                    let count = subscriber.poll(&mut |_fragment: &[u8]| {}).unwrap_or(0);
                    total += count;
                    if count == 0 {
                        thread::yield_now();
                    }
                }

                black_box(total)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
