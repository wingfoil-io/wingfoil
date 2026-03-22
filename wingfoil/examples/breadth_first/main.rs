#![doc = include_str!("./README.md")]

use wingfoil::*;

fn main() {
    env_logger::init();
    let mut source = constant(1_u128);
    for _ in 1..128 {
        source = add(&source, &source);
    }
    source
        .timed()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    println!("value {:?}", source.peek_value());
}
