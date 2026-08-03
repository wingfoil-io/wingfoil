#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let wf_anon_1 = g.ticker(Duration::from_nanos(10)).count();
        wf_anon_1
    }
}

fn main() {}
