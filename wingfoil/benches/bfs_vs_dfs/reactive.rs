// Demonstrates O(2^N) depth-first execution in a reactive (rxrust) framework.
//
// Each level wires combine_latest(prev, prev): one source emission fires both
// arms, producing 2 downstream emissions. Across N levels this becomes 2^N.

use criterion::{Criterion, criterion_group, criterion_main};
use rxrust::prelude::*;
use std::any::Any;
use std::convert::Infallible;
use std::hint::black_box;

fn bench(crit: &mut Criterion) {
    for depth in 1..=10 {
        let mut root: LocalSubject<'static, u128, Infallible> = Local::subject();
        let mut current = root.clone();

        // Each subscription must be kept alive or rxrust drops it.
        // Types differ per level so we erase them into Box<dyn Any>.
        let mut subs: Vec<Box<dyn Any>> = Vec::new();

        for _ in 0..depth {
            let next: LocalSubject<'static, u128, Infallible> = Local::subject();
            let mut emitter = next.clone();
            subs.push(Box::new(
                current
                    .clone()
                    .combine_latest(current.clone(), |a: u128, b: u128| a + b)
                    .subscribe(move |v| emitter.next(v)),
            ));
            current = next;
        }
        subs.push(Box::new(current.subscribe(|v| {
            black_box(v);
        })));

        crit.bench_function(&format!("depth_{depth}"), |b| {
            b.iter(|| {
                root.next(black_box(1u128));
                root.next(black_box(1u128));
            });
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
