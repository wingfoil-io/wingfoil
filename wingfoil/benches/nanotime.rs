use criterion::{Criterion, criterion_group, criterion_main};
use wingfoil::NanoTime;

fn bench(crit: &mut Criterion) {
    crit.bench_function("nanotime", |bencher| bencher.iter(NanoTime::now));
}

criterion_group!(benches, bench);
criterion_main!(benches);
