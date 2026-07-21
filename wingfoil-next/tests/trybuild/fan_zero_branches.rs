#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

// `fan` must have at least one branch — a zero-way merge has no output.
wingfoil_next::graph! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).count().fan(0, |s| s.map(|i| *i + 1));
        out
    }
}

fn main() {}
