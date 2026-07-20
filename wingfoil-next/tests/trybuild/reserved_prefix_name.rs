#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

wingfoil_next::graph! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let __wf_anon1 = g.ticker(Duration::from_nanos(10)).count();
        __wf_anon1
    }
}

fn main() {}
