// A tuple-bound call is a composite, so `nitro!` asks the composite's shell
// for its body. With no shell in scope that is the one error, spanned at the
// method — the macro it names says what to define (or import) to fix it.
#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(Duration::from_nanos(10)).count();
        let (low, high) = count.frobnicate(3);
        low
    }
}

fn main() {}
