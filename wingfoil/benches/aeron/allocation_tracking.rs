//! Allocation tracking benchmarks for Aeron transport.
//!
//! Verifies zero-allocation behavior in hot paths using dhat for
//! allocation profiling. This helps ensure the transport layer
//! maintains its performance guarantees.

#[path = "common/mod.rs"]
mod common;

use common::MessageSize;
use common::rusteron_support::BenchContext;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::thread;
use std::time::Duration;
use wingfoil::adapters::aeron::error::TransportError;
use wingfoil::adapters::aeron::rusteron_backend::{RusteronPublisher, RusteronSubscriber};
use wingfoil::adapters::aeron::{AeronPublisherBackend, AeronSubscriberBackend};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Benchmark publication hot path allocations.
fn bench_publication_allocations(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let mut group = c.benchmark_group("allocations/publication");

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let stream_id = 11001 + size.bytes() as i32;
        let publication = ctx.add_publication(stream_id);
        let mut publisher = RusteronPublisher::new(publication);

        let buffer = size.create_buffer();

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            b.iter(|| {
                // Measure allocations during offer
                let _ = publisher.offer(&buffer);
            });
        });
    }

    group.finish();
}

/// Benchmark try_claim + commit hot path allocations.
///
/// Mirrors `bench_publication_allocations` but exercises the zero-copy
/// `try_claim → data → commit` path instead of `offer`. The deltas captured
/// under `--features dhat-heap` should be no worse than `offer`'s, with the
/// expected (but not guaranteed) win being one fewer per-iteration allocation
/// because no `&[u8]` argument copy crosses the FFI boundary.
fn bench_try_claim_allocations(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let mut group = c.benchmark_group("allocations/try_claim");

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let stream_id = 12001 + size.bytes() as i32;
        let publication = ctx.add_publication(stream_id);
        let mut publisher = RusteronPublisher::new(publication);

        let payload = size.create_buffer();

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            b.iter(|| {
                match publisher.try_claim(payload.len()) {
                    Ok(mut claim) => {
                        claim.data().copy_from_slice(&payload);
                        let _ = claim.commit();
                    }
                    Err(TransportError::Invalid(_)) => {
                        // Backend may not support try_claim — skip.
                    }
                    Err(_) => {
                        // BackPressure / not-connected: still represents an
                        // allocation profile we want to capture honestly.
                    }
                }
            });
        });
    }

    group.finish();
}

/// Benchmark subscription hot path allocations.
fn bench_subscription_allocations(c: &mut Criterion) {
    let ctx = match BenchContext::new() {
        Some(c) => c,
        None => return,
    };

    let mut group = c.benchmark_group("allocations/subscription");

    for size in [MessageSize::Small, MessageSize::Medium, MessageSize::Large] {
        let stream_id = 13001 + size.bytes() as i32;
        let (publication, subscription) = ctx.add_pub_sub(stream_id);

        let mut publisher = RusteronPublisher::new(publication);
        let mut subscriber = RusteronSubscriber::new(subscription);

        // Pre-publish some messages
        let buffer = size.create_buffer();
        for _ in 0..100 {
            let _ = publisher.offer(&buffer);
        }
        thread::sleep(Duration::from_millis(50));

        group.bench_function(BenchmarkId::from_parameter(size.name()), |b| {
            b.iter(|| {
                // Measure allocations during poll
                let _ = subscriber.poll(&mut |_fragment: &[u8]| {});
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_publication_allocations,
    bench_try_claim_allocations,
    bench_subscription_allocations
);
criterion_main!(benches);
