// Demonstrates O(2^N) depth-first execution with async.
//
// Each level awaits the left branch then the right branch independently —
// the same source value flows down both paths separately. This is the async
// equivalent of rxrust's combine_latest(src, src): one tick produces 2^N
// sequential awaits across N levels.

use criterion::{Criterion, criterion_group, criterion_main};
use std::future::Future;
use std::hint::black_box;
use std::pin::Pin;
use tokio::runtime::Runtime;

fn branch_recombine(depth: usize, value: u128) -> Pin<Box<dyn Future<Output = u128>>> {
    Box::pin(async move {
        if depth == 0 {
            return black_box(value);
        }
        let left = branch_recombine(depth - 1, value).await;
        let right = branch_recombine(depth - 1, value).await;
        left + right
    })
}

fn bench(crit: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    for depth in 1..=10 {
        crit.bench_function(&format!("depth_{depth}"), |b| {
            b.iter(|| rt.block_on(branch_recombine(depth, black_box(1u128))));
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
