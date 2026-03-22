use criterion::{Criterion, criterion_group, criterion_main};
use std::rc::Rc;
use wingfoil::{Node, NodeOperators, StreamOperators, add, add_bench};

fn branch_recombine(trig: Rc<dyn Node>, depth: usize) -> Rc<dyn Node> {
    let mut source = trig.count().map(|_: u64| 1_u128);
    for _ in 0..depth {
        source = add(&source, &source);
    }
    source.as_node()
}

fn bench(crit: &mut Criterion) {
    for depth in 1..=10 {
        add_bench(crit, &format!("depth_{depth}"), move |trig| {
            branch_recombine(trig, depth)
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
