#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn bad<T>(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).count();
        out
    }
}

fn main() {}
