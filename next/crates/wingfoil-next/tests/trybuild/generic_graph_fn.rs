#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

wingfoil_next::graph! {
    fn bad<T>(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).count();
        out
    }
}

fn main() {}
