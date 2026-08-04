#![doc = include_str!("./README.md")]

use wingfoil::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut source = constant(1_u128);
    for _ in 1..128 {
        source = add(&source, &source);
    }
    source.timed().graph().historical().forever().run()?;
    println!("value {:?}", source.peek_value());
    Ok(())
}
