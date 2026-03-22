#![doc = include_str!("./README.md")]

use wingfoil::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut source = constant(1_u128);
    for _ in 1..128 {
        source = add(&source, &source);
    }
    source
        .timed()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    println!("value {:?}", source.peek_value());
    Ok(())
}
