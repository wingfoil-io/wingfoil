use criterion::{Criterion, criterion_group, criterion_main};
use std::rc::Rc;
use wingfoil::{Node, NodeOperators, StreamOperators, add_bench, merge};

fn node(trig: Rc<dyn Node>) -> Rc<dyn Node> {
    trig
}

fn nodes(trig: Rc<dyn Node>, width: usize, depth: usize) -> Rc<dyn Node> {
    let src = trig.count();
    let streams = (0..width)
        .map(|_| {
            let mut stream = src.clone();
            for _ in 0..depth {
                stream = stream.map(std::hint::black_box);
            }
            stream
        })
        .collect::<Vec<_>>();
    merge(streams)
}

fn bench(crit: &mut Criterion) {
    add_bench(crit, "node", node);
    add_bench(crit, "10x10", |trig| nodes(trig, 10, 10));
    add_bench(crit, "100x100", |trig| nodes(trig, 100, 100));
}

criterion_group!(benches, bench);
criterion_main!(benches);
