#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

// `map_n`'s count must be a literal so the unrolled DAG is static; a runtime
// value cannot be unrolled at expansion time.
wingfoil_next::graph! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let n = 3;
        let out = g.ticker(Duration::from_nanos(10)).count().map_n(n, |i| *i + 1);
        out
    }
}

fn main() {}
