#![doc = include_str!("./README.md")]

use wingfoil::*;

fn main() {
    let mut source = constant(1_u128);
    for _ in 1..128 {
        source = add(&source, &source);
    }
    let cycles = source.count();
    cycles
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    println!("cycles {:?}", cycles.peek_value());
    println!("value {:?}", source.peek_value());
}
