// Demonstrates O(2^N) depth-first execution in a reactive (rxrust) framework.
//
// Each level wires combine_latest(prev, prev): one source emission fires both
// arms, producing 2 downstream emissions. Across N levels this becomes 2^N.

use criterion::{Criterion, criterion_group, criterion_main};
use rxrust::prelude::*;
use std::convert::Infallible;
use std::hint::black_box;
use std::sync::{Arc, Mutex};

/// Build a chain of depth N using combine_latest(src, src) at each level.
/// Returns (root_subject, leaf_count, _subscriptions_keepalive).
fn build_chain(
    depth: usize,
) -> (
    LocalSubject<'static, u128, Infallible>,
    Arc<Mutex<u64>>,
    Vec<Box<dyn std::any::Any>>,
) {
    let root: LocalSubject<'static, u128, Infallible> = Local::subject();
    let mut current = root.clone();
    let mut keepalive: Vec<Box<dyn std::any::Any>> = Vec::new();

    for _ in 0..depth {
        let next: LocalSubject<'static, u128, Infallible> = Local::subject();
        let mut next_emitter = next.clone();
        let sub = current
            .clone()
            .combine_latest(current.clone(), |a: u128, b: u128| a + b)
            .subscribe(move |v| next_emitter.next(v));
        keepalive.push(Box::new(sub));
        current = next;
    }

    let count = Arc::new(Mutex::new(0u64));
    let count_clone = count.clone();
    let sub = current.subscribe(move |v| {
        *count_clone.lock().unwrap() += black_box(v) as u64;
    });
    keepalive.push(Box::new(sub));

    (root, count, keepalive)
}

fn bench_depth(crit: &mut Criterion, depth: usize) {
    let (mut root, _count, _keepalive) = build_chain(depth);
    crit.bench_function(&format!("depth_{depth}"), |b| {
        b.iter(|| {
            root.next(black_box(1u128));
            root.next(black_box(1u128));
        });
    });
}

fn bench(crit: &mut Criterion) {
    for depth in 1..=10 {
        bench_depth(crit, depth);
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
