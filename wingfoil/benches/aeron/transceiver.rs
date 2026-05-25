//! Combined pub/sub (transceiver) benchmarks for Aeron transport.
//!
//! Measures performance when publishing and subscribing concurrently,
//! including request/response roundtrip latency and bidirectional exchange.

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

/// Benchmark simultaneous publish/subscribe on different streams.
fn bench_simultaneous_pub_sub(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let mut group = c.benchmark_group("transceiver/simultaneous");

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let publication = ctx.add_publication(7001);
        let subscription = ctx.add_subscription(7002);

        let mut publisher = RusteronPublisher::new(publication);
        let mut subscriber = RusteronSubscriber::new(subscription);

        group.throughput(Throughput::Bytes(size.bytes() as u64 * 2)); // pub + sub

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            let buffer = size.create_buffer();

            b.iter(|| {
                // Publish
                let pub_result = publisher.offer(&buffer);

                // Poll (may or may not have messages)
                let poll_result = subscriber.poll(&mut |_fragment: &[u8]| {});

                black_box((pub_result, poll_result))
            });
        });
    }

    group.finish();
}

/// Benchmark request/response roundtrip latency.
fn bench_request_response(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let mut group = c.benchmark_group("transceiver/roundtrip");

    for size in [MessageSize::Small, MessageSize::Medium] {
        // Client: publishes requests, subscribes to responses
        let client_pub = ctx.add_publication(8001);
        let client_sub = ctx.add_subscription(8002);
        let mut client_publisher = RusteronPublisher::new(client_pub);
        let mut client_subscriber = RusteronSubscriber::new(client_sub);

        // Server: subscribes to requests, publishes responses
        let server_sub = ctx.add_subscription(8001);
        let server_pub = ctx.add_publication(8002);
        let mut server_subscriber = RusteronSubscriber::new(server_sub);
        let mut server_publisher = RusteronPublisher::new(server_pub);

        thread::sleep(Duration::from_millis(100));

        group.throughput(Throughput::Elements(1));

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            let request = size.create_buffer();
            let response = size.create_buffer();

            b.iter(|| {
                // Client sends request
                while client_publisher.offer(&request).is_err() {
                    thread::yield_now();
                }

                // Server polls for request
                let mut received_request = false;
                for _ in 0..1000 {
                    let count = server_subscriber
                        .poll(&mut |_fragment: &[u8]| {})
                        .unwrap_or(0);
                    if count > 0 {
                        received_request = true;
                        break;
                    }
                    thread::yield_now();
                }

                if received_request {
                    // Server sends response
                    while server_publisher.offer(&response).is_err() {
                        thread::yield_now();
                    }

                    // Client polls for response
                    for _ in 0..1000 {
                        let count = client_subscriber
                            .poll(&mut |_fragment: &[u8]| {})
                            .unwrap_or(0);
                        if count > 0 {
                            break;
                        }
                        thread::yield_now();
                    }
                }
            });
        });
    }

    group.finish();
}

/// Benchmark bidirectional symmetric exchange.
fn bench_bidirectional(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let mut group = c.benchmark_group("transceiver/bidirectional");

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        // Side A: pub to B, sub from B.
        // Stream IDs in the 14000+ range keep benches disjoint from the
        // 9000+ range that examples (Story 12.7 Dev Notes) reserve.
        let pub_a = ctx.add_publication(14001);
        let sub_a = ctx.add_subscription(14002);
        let mut publisher_a = RusteronPublisher::new(pub_a);
        let mut subscriber_a = RusteronSubscriber::new(sub_a);

        // Side B: pub to A, sub from A
        let pub_b = ctx.add_publication(14002);
        let sub_b = ctx.add_subscription(14001);
        let mut publisher_b = RusteronPublisher::new(pub_b);
        let mut subscriber_b = RusteronSubscriber::new(sub_b);

        thread::sleep(Duration::from_millis(100));

        group.throughput(Throughput::Bytes(size.bytes() as u64 * 2));

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            let buffer_a = size.create_buffer();
            let buffer_b = size.create_buffer();

            b.iter(|| {
                // Both sides publish
                let _ = publisher_a.offer(&buffer_a);
                let _ = publisher_b.offer(&buffer_b);

                // Both sides poll
                let _ = subscriber_a.poll(&mut |_fragment: &[u8]| {});
                let _ = subscriber_b.poll(&mut |_fragment: &[u8]| {});
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_simultaneous_pub_sub,
    bench_request_response,
    bench_bidirectional
);
criterion_main!(benches);
